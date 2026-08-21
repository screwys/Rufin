use std::cell::{Cell, RefCell};
use std::io::{self, Write};
use std::ops::ControlFlow;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::Duration;

use adw::prelude::*;
use app_identity::{APP_ID, DISPLAY_NAME, STABLE_APP_ID};
use gtk::gio;
use tracing::error;

use crate::runtime::RuntimeInputs;

pub(crate) mod style;

const ICON_RESOURCE_ROOT: &str = "/io/github/screwys/Rufin/icons/hicolor";
const GTK_DECORATION_LAYOUT_OPTION: &str = "gtk-decoration-layout";
const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);
const WINDOW_BAR_PREVIEW_OPTION: &str = "window-bar-preview";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WindowBarPreview {
    Macos,
    Windows,
}

impl WindowBarPreview {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "macos" => Ok(Self::Macos),
            "windows" => Ok(Self::Windows),
            _ => Err(format!(
                "Unknown window bar preview '{value}'. Use 'macos' or 'windows'."
            )),
        }
    }
}

#[derive(Default)]
struct ApplicationOptions {
    decoration_layout: Option<String>,
    window_bar_preview: Option<WindowBarPreview>,
}

pub fn run_application<F>(bootstrap: F) -> ExitCode
where
    F: FnOnce() -> Result<RuntimeInputs, String> + 'static,
{
    run_application_with_presentation(bootstrap, false, None)
}

pub fn run_application_after_update<F>(bootstrap: F, presented: impl FnOnce() + 'static) -> ExitCode
where
    F: FnOnce() -> Result<RuntimeInputs, String> + 'static,
{
    run_application_with_presentation(bootstrap, true, Some(Box::new(presented)))
}

fn run_application_with_presentation<F>(
    bootstrap: F,
    force_initial_presentation: bool,
    presented: Option<Box<dyn FnOnce()>>,
) -> ExitCode
where
    F: FnOnce() -> Result<RuntimeInputs, String> + 'static,
{
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("rufin-async")
        .build();
    let runtime = match runtime {
        Ok(runtime) => runtime,
        Err(error) => {
            error!(%error, "failed to create async runtime");
            return run_startup_error_application(error.to_string());
        }
    };
    let runtime_guard = runtime.enter();

    let (app, options) = application();
    connect_startup_configuration(&app, Rc::clone(&options));
    let bootstrap = Rc::new(RefCell::new(Some(bootstrap)));
    let presented = Rc::new(RefCell::new(presented));
    let quitting = Rc::new(Cell::new(false));
    app.connect_activate(move |app| {
        if quitting.get() {
            return;
        }
        if let Some(window) = app
            .active_window()
            .or_else(|| app.windows().into_iter().next())
        {
            present_window(&window);
            return;
        }
        let Some(bootstrap) = bootstrap.borrow_mut().take() else {
            return;
        };
        match bootstrap() {
            Ok(inputs) => {
                let window_bar_preview = options.borrow().window_bar_preview;
                crate::shell::build::build(
                    app,
                    inputs,
                    Rc::clone(&quitting),
                    force_initial_presentation,
                    presented.borrow_mut().take(),
                    window_bar_preview,
                )
            }
            Err(error) => {
                error!(%error, "failed to start Rufin");
                present_startup_error(app, &error, options.borrow().window_bar_preview);
            }
        }
    });

    let exit_code: ExitCode = app.run().into();
    drop(app);
    drop(runtime_guard);
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
    exit_code
}

fn run_startup_error_application(error: String) -> ExitCode {
    let (app, options) = application();
    connect_startup_configuration(&app, Rc::clone(&options));
    app.connect_activate(move |app| {
        if let Some(window) = app.active_window() {
            present_window(&window);
        } else {
            present_startup_error(app, &error, options.borrow().window_bar_preview);
        }
    });
    app.run().into()
}

fn application() -> (adw::Application, Rc<RefCell<ApplicationOptions>>) {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::empty())
        .build();
    app.add_main_option(
        GTK_DECORATION_LAYOUT_OPTION,
        0u8.into(),
        gtk::glib::OptionFlags::NONE,
        gtk::glib::OptionArg::String,
        "Override GTK's window control layout for this run",
        Some("LAYOUT"),
    );
    app.add_main_option(
        WINDOW_BAR_PREVIEW_OPTION,
        0u8.into(),
        gtk::glib::OptionFlags::NONE,
        gtk::glib::OptionArg::String,
        "Render a 30px platform window bar for alignment inspection",
        Some("macos|windows"),
    );
    let options = Rc::new(RefCell::new(ApplicationOptions::default()));
    let requested_options = Rc::clone(&options);
    app.connect_handle_local_options(move |_, options| {
        if let Ok(Some(layout)) = options.lookup::<String>(GTK_DECORATION_LAYOUT_OPTION) {
            requested_options.borrow_mut().decoration_layout = Some(layout);
        }
        if let Ok(Some(value)) = options.lookup::<String>(WINDOW_BAR_PREVIEW_OPTION) {
            match WindowBarPreview::parse(&value) {
                Ok(preview) => requested_options.borrow_mut().window_bar_preview = Some(preview),
                Err(message) => {
                    let _ = writeln!(io::stderr().lock(), "{message}");
                    return ControlFlow::Break(gtk::glib::ExitCode::FAILURE);
                }
            }
        }
        ControlFlow::Continue(())
    });

    let quit = gio::SimpleAction::new("quit", None);
    let quit_app = app.clone();
    quit.connect_activate(move |_, _| quit_app.quit());
    app.add_action(&quit);
    #[cfg(target_os = "macos")]
    {
        app.set_accels_for_action("app.quit", &["<Meta>q"]);
        app.set_accels_for_action("window.close", &["<Meta>w"]);
    }
    #[cfg(not(target_os = "macos"))]
    {
        app.set_accels_for_action("app.quit", &["<Control>q"]);
        app.set_accels_for_action("window.close", &["<Control>w"]);
    }
    (app, options)
}

fn connect_startup_configuration(app: &adw::Application, options: Rc<RefCell<ApplicationOptions>>) {
    app.connect_startup(move |_| {
        configure_app_icon();
        let requested_options = options.borrow();
        let Some(layout) = requested_options.decoration_layout.as_deref() else {
            return;
        };
        let Some(settings) = gtk::Settings::default() else {
            error!("could not apply the requested GTK window control layout");
            return;
        };
        settings.set_gtk_decoration_layout(Some(layout));
    });
}

pub(crate) fn present_window(window: &impl IsA<gtk::Window>) {
    let window = window.as_ref();
    let had_focus = gtk::prelude::RootExt::focus(window).is_some();
    window.present();
    if !had_focus {
        gtk::prelude::RootExt::set_focus(window, None::<&gtk::Widget>);
    }
}

pub(crate) fn application_window(
    app: &adw::Application,
    title: &str,
    default_width: i32,
    default_height: i32,
    content: &impl IsA<gtk::Widget>,
    preview: Option<WindowBarPreview>,
) -> gtk::ApplicationWindow {
    if let Some(platform) = platform_window_bar(preview) {
        let window = gtk_application_window(app, title, default_width, default_height, content);
        install_platform_window_bar(&window, platform, preview.is_some());
        return window;
    }

    adw::ApplicationWindow::builder()
        .application(app)
        .title(title)
        .default_width(default_width)
        .default_height(default_height)
        .content(content)
        .build()
        .upcast::<gtk::ApplicationWindow>()
}

pub(crate) fn platform_window_bar(preview: Option<WindowBarPreview>) -> Option<WindowBarPreview> {
    preview.or({
        #[cfg(target_os = "macos")]
        {
            Some(WindowBarPreview::Macos)
        }
        #[cfg(target_os = "windows")]
        {
            Some(WindowBarPreview::Windows)
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            None
        }
    })
}

fn gtk_application_window(
    app: &adw::Application,
    title: &str,
    default_width: i32,
    default_height: i32,
    content: &impl IsA<gtk::Widget>,
) -> gtk::ApplicationWindow {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title(title)
        .default_width(default_width)
        .default_height(default_height)
        .build();
    window.set_child(Some(content));
    window
}

fn install_platform_window_bar(
    window: &gtk::ApplicationWindow,
    platform: WindowBarPreview,
    preview: bool,
) {
    let titlebar = gtk::HeaderBar::new();
    titlebar.add_css_class("platform-window-bar");
    titlebar.set_height_request(30);
    titlebar.set_show_title_buttons(!preview);
    titlebar.set_use_native_controls(platform == WindowBarPreview::Macos && !preview);
    titlebar.set_title_widget(Some(&bound_window_title(
        window,
        "platform-window-bar-title",
    )));

    match platform {
        WindowBarPreview::Macos => {
            if preview {
                titlebar.pack_start(&macos_preview_controls());
            }
        }
        WindowBarPreview::Windows => {
            if preview {
                titlebar.pack_end(&windows_preview_controls());
            }
        }
    }

    window.set_titlebar(Some(&titlebar));
}

fn bound_window_title(window: &gtk::ApplicationWindow, css_class: &str) -> gtk::Label {
    let title = gtk::Label::new(None);
    title.add_css_class("heading");
    title.add_css_class(css_class);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_single_line_mode(true);
    window
        .bind_property("title", &title, "label")
        .sync_create()
        .build();
    title
}

fn macos_preview_controls() -> gtk::Box {
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    controls.add_css_class("macos-window-bar-preview-controls");
    controls.set_valign(gtk::Align::Center);
    for class in ["close", "minimize", "maximize"] {
        let control = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        control.add_css_class("macos-window-bar-preview-control");
        control.add_css_class(class);
        control.set_size_request(12, 12);
        control.set_halign(gtk::Align::Center);
        control.set_valign(gtk::Align::Center);
        controls.append(&control);
    }
    controls
}

fn windows_preview_controls() -> gtk::Box {
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    controls.add_css_class("windows-window-bar-preview-controls");
    for (label, class) in [("−", "minimize"), ("□", "maximize"), ("×", "close")] {
        let control = gtk::Label::new(Some(label));
        control.add_css_class("windows-window-bar-preview-control");
        control.add_css_class(class);
        controls.append(&control);
    }
    controls
}

fn present_startup_error(app: &adw::Application, error: &str, preview: Option<WindowBarPreview>) {
    let status = adw::StatusPage::builder()
        .icon_name(STABLE_APP_ID)
        .title(DISPLAY_NAME)
        .description(error)
        .build();
    let window = application_window(app, DISPLAY_NAME, 480, 320, &status, preview);
    present_window(&window);
}

fn configure_app_icon() {
    if let Err(error) = register_resources() {
        error!(%error, "failed to register Rufin's interface resources");
    }
    gtk::Window::set_default_icon_name(STABLE_APP_ID);
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    gtk::IconTheme::for_display(&display).add_resource_path(ICON_RESOURCE_ROOT);
}

fn register_resources() -> Result<(), String> {
    static REGISTERED: OnceLock<Result<(), String>> = OnceLock::new();
    REGISTERED
        .get_or_init(|| {
            gio::resources_register_include!("rufin.gresource").map_err(|error| error.to_string())
        })
        .clone()
}

pub(crate) fn verify_interface_resources() -> Result<(), String> {
    register_resources()?;
    for relative_path in [
        "scalable/apps/io.github.screwys.Rufin.svg",
        "scalable/actions/rufin-play-symbolic.svg",
        "scalable/actions/rufin-open-menu-symbolic.svg",
        "scalable/actions/rufin-x-office-calendar-symbolic.svg",
        "symbolic/apps/io.github.screwys.Rufin-symbolic.svg",
    ] {
        let path = format!("{ICON_RESOURCE_ROOT}/{relative_path}");
        gio::resources_lookup_data(&path, gio::ResourceLookupFlags::NONE)
            .map_err(|error| format!("missing compiled Rufin resource {path}: {error}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_bar_preview_accepts_only_named_platforms() {
        assert_eq!(
            WindowBarPreview::parse("macos"),
            Ok(WindowBarPreview::Macos)
        );
        assert_eq!(
            WindowBarPreview::parse("windows"),
            Ok(WindowBarPreview::Windows)
        );
        assert!(WindowBarPreview::parse("linux").is_err());
    }

    #[test]
    fn representative_rufin_icons_are_compiled_resources() {
        verify_interface_resources().expect("compiled interface resources");
    }
}
