use crate::Preferences;
use downloads::{DownloadQueueItem, DownloadQueueSnapshot};
use sources::SourceId;
use std::{rc::Rc, sync::Arc};
impl Preferences {
    pub fn move_download_job(
        self: &Rc<Self>,
        source_id: SourceId,
        job_id: String,
        target_job_id: String,
        after: bool,
    ) -> bool {
        {
            let mut snapshots = self.downloads.snapshots.borrow_mut();
            let Some(snapshot) = snapshots.get_mut(&Some(source_id.clone())) else {
                return false;
            };
            let mut jobs = snapshot.jobs.to_vec();
            if !reorder_queue_items(&mut jobs, &job_id, &target_job_id, after) {
                return false;
            }
            *snapshot = Arc::new(DownloadQueueSnapshot {
                jobs: jobs.into(),
                downloaded_tracks: snapshot.downloaded_tracks,
                paused: snapshot.paused,
            });
        }
        self.downloads.refresh_queue();
        self.products
            .downloads
            .move_job(source_id, job_id, target_job_id, after);
        true
    }
}
fn reorder_queue_items(
    jobs: &mut Vec<DownloadQueueItem>,
    job_id: &str,
    target_job_id: &str,
    after: bool,
) -> bool {
    if job_id == target_job_id {
        return false;
    }
    let Some(source_index) = jobs.iter().position(|job| job.id == job_id) else {
        return false;
    };
    let job = jobs.remove(source_index);
    let Some(target_index) = jobs
        .iter()
        .position(|candidate| candidate.id == target_job_id)
    else {
        jobs.insert(source_index, job);
        return false;
    };
    jobs.insert(target_index + usize::from(after), job);
    true
}
