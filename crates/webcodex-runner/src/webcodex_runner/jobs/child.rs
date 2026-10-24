//! One existing Job child slot, with no ambient fallback for isolated children.
use std::io;
use std::process::ExitStatus;
use std::time::Duration;
use webcodex_process::{GracefulTermination, ManagedChild};

pub(crate) enum JobChild {
    Managed(ManagedChild),
    #[cfg(windows)]
    Isolated(webcodex_process::windows_lpac::LpacChild),
}

impl std::fmt::Debug for JobChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobChild").field("id", &self.id()).finish()
    }
}

impl From<ManagedChild> for JobChild {
    fn from(value: ManagedChild) -> Self {
        Self::Managed(value)
    }
}

impl JobChild {
    pub(crate) fn id(&self) -> u32 {
        match self {
            Self::Managed(child) => child.id(),
            #[cfg(windows)]
            Self::Isolated(child) => child.id(),
        }
    }

    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        match self {
            Self::Managed(child) => child.try_wait(),
            #[cfg(windows)]
            Self::Isolated(child) => {
                use std::os::windows::process::ExitStatusExt;
                child
                    .try_wait()
                    .map(|status| status.map(ExitStatus::from_raw))
            }
        }
    }

    pub(crate) fn terminate_tree(&mut self) -> io::Result<()> {
        match self {
            Self::Managed(child) => child.terminate_tree(),
            #[cfg(windows)]
            Self::Isolated(child) => child.terminate_tree(),
        }
    }

    pub(crate) fn request_terminate_tree(&mut self) -> io::Result<GracefulTermination> {
        match self {
            Self::Managed(child) => child.request_terminate_tree(),
            #[cfg(windows)]
            Self::Isolated(child) => child
                .terminate_tree()
                .map(|_| GracefulTermination::Requested),
        }
    }

    // Compatibility cleanup only. These methods never generate an isolated
    // close receipt: that requires LpacChild::observe_completed_tree and the
    // separate worker/output completion proof.
    pub(crate) fn try_tree_exit(&mut self) -> io::Result<bool> {
        match self {
            Self::Managed(child) => child.try_tree_exit(),
            #[cfg(windows)]
            Self::Isolated(child) => child.active_processes().map(|count| count == 0),
        }
    }

    pub(crate) fn wait_tree_exit(&mut self, timeout: Duration) -> io::Result<bool> {
        match self {
            Self::Managed(child) => child.wait_tree_exit(timeout),
            #[cfg(windows)]
            Self::Isolated(child) => child.wait_tree_exit(timeout),
        }
    }
}
