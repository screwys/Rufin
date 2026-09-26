use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

use gtk::{gdk, glib, prelude::*};

use super::ArtworkTile;

#[derive(Default)]
pub(super) struct Animations {
    sessions: HashMap<artwork::ArtworkKey, Weak<Session>>,
}

pub(super) struct Target {
    pub area: glib::WeakRef<gtk::Overlay>,
    pub picture: glib::WeakRef<gtk::Picture>,
    pub generation: Rc<Cell<u64>>,
    pub expected_generation: u64,
    pub size: Rc<Cell<i32>>,
    pub scale: Cell<f64>,
}

pub(super) struct Session {
    bytes: RefCell<Option<Arc<[u8]>>>,
    targets: RefCell<Vec<Rc<Target>>>,
    render_size: Cell<u32>,
    current: RefCell<gdk::Texture>,
    task: RefCell<Option<glib::JoinHandle<()>>>,
}

pub(super) struct Lease {
    pub session: Rc<Session>,
    pub target: Rc<Target>,
}

impl Animations {
    pub fn attach(
        &mut self,
        key: artwork::ArtworkKey,
        bytes: Arc<[u8]>,
        tile: &ArtworkTile,
        generation: u64,
        scale: f64,
        poster: gdk::Texture,
    ) -> Lease {
        self.sessions
            .retain(|_, session| session.strong_count() > 0);
        let session = self
            .sessions
            .get(&key)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| {
                let session = Rc::new(Session {
                    bytes: RefCell::new(Some(bytes)),
                    targets: RefCell::new(Vec::new()),
                    render_size: Cell::new(0),
                    current: RefCell::new(poster),
                    task: RefCell::new(None),
                });
                self.sessions.insert(key, Rc::downgrade(&session));
                session
            });
        session.attach(tile, generation, scale)
    }

    pub fn existing(
        &self,
        key: &artwork::ArtworkKey,
        tile: &ArtworkTile,
        generation: u64,
        scale: f64,
    ) -> Option<(Lease, gdk::Texture)> {
        let session = self.sessions.get(key)?.upgrade()?;
        let texture = session.current.borrow().clone();
        Some((session.attach(tile, generation, scale), texture))
    }
}

impl Session {
    fn attach(self: &Rc<Self>, tile: &ArtworkTile, generation: u64, scale: f64) -> Lease {
        let target = Rc::new(tile.animation_target(generation, scale));
        self.targets.borrow_mut().push(Rc::clone(&target));
        Lease {
            session: Rc::clone(self),
            target,
        }
    }
}

impl Target {
    fn mapped(&self) -> bool {
        self.generation.get() == self.expected_generation
            && self.area.upgrade().is_some_and(|area| area.is_mapped())
    }
}

impl Session {
    pub fn refresh(self: &Rc<Self>) {
        let current = self.current.borrow().clone();
        self.present(&current);
        let size = self
            .targets
            .borrow()
            .iter()
            .filter(|target| target.mapped())
            .map(|target| {
                super::cover_decode_size(
                    target.size.get(),
                    super::LARGE_COVER_SIZE,
                    target.scale.get(),
                )
            })
            .max()
            .unwrap_or(0);
        if self.render_size.replace(size) == size {
            return;
        }
        if size == 0 {
            if let Some(task) = self.task.borrow_mut().take() {
                task.abort();
            }
            return;
        }
        if self
            .task
            .borrow()
            .as_ref()
            .is_some_and(|task| !task.source().is_destroyed())
        {
            return;
        }
        self.task.borrow_mut().take();

        let (request_tx, request_rx) = async_channel::bounded::<u32>(1);
        let (frame_tx, frame_rx) = async_channel::bounded(1);
        let Some(bytes) = self.bytes.borrow().clone() else {
            return;
        };
        std::thread::spawn(move || {
            let Ok(mut size) = request_rx.recv_blocking() else {
                return;
            };
            let mut animation = match artwork::Animation::new(bytes, size) {
                Ok(Some(animation)) => animation,
                Ok(None) => {
                    let _ = frame_tx.send_blocking(None);
                    return;
                }
                Err(error) => {
                    tracing::warn!(%error, "cover animation could not be opened");
                    return;
                }
            };
            loop {
                animation.set_render_size(size);
                match animation.next_frame() {
                    Ok(Some(frame)) => {
                        if frame_tx.send_blocking(Some(frame)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        tracing::warn!(%error, "cover animation could not be decoded");
                        break;
                    }
                }
                let Ok(next_size) = request_rx.recv_blocking() else {
                    break;
                };
                size = next_size;
            }
        });
        let session = Rc::downgrade(self);
        self.task.replace(Some(glib::spawn_future_local(async move {
            if request_tx.send(size).await.is_err() {
                return;
            }
            loop {
                let Ok(frame) = frame_rx.recv().await else {
                    break;
                };
                let Some(frame) = frame else {
                    if let Some(session) = session.upgrade() {
                        session.bytes.borrow_mut().take();
                    }
                    break;
                };
                let width = frame.pixels.width() as i32;
                let height = frame.pixels.height() as i32;
                let bytes = glib::Bytes::from_owned(frame.pixels);
                let texture: gdk::Texture = gdk::MemoryTexture::new(
                    width,
                    height,
                    gdk::MemoryFormat::R8g8b8a8,
                    &bytes,
                    width as usize * 4,
                )
                .upcast();
                let size = {
                    let Some(session) = session.upgrade() else {
                        break;
                    };
                    session.current.replace(texture.clone());
                    session.present(&texture);
                    session.render_size.get()
                };
                if request_tx.send(size).await.is_err() {
                    break;
                }
                let duration = if frame.duration.is_zero() {
                    Duration::from_millis(100)
                } else {
                    frame.duration
                };
                glib::timeout_future(duration).await;
            }
        })));
    }

    fn present(&self, texture: &gdk::Texture) {
        let pictures: Vec<_> = self
            .targets
            .borrow()
            .iter()
            .filter(|target| target.mapped())
            .filter_map(|target| target.picture.upgrade())
            .collect();
        for picture in pictures {
            picture.set_paintable(Some(texture));
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.session
            .targets
            .borrow_mut()
            .retain(|target| !Rc::ptr_eq(target, &self.target));
        self.session.refresh();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(task) = self.task.get_mut().take() {
            task.abort();
        }
    }
}
