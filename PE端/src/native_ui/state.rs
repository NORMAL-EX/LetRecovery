use crate::core::config::OperationType;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NativePage {
    #[default]
    Overview,
    Progress,
    AdvancedOptions,
    Error,
    Recovery,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WorkflowKind {
    Install,
    Backup,
    Expand,
    #[default]
    Missing,
}

impl From<Option<OperationType>> for WorkflowKind {
    fn from(value: Option<OperationType>) -> Self {
        match value {
            Some(OperationType::Install) => Self::Install,
            Some(OperationType::Backup) => Self::Backup,
            Some(OperationType::Expand) => Self::Expand,
            None => Self::Missing,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandBarState {
    pub back_visible: bool,
    pub primary_enabled: bool,
    pub cancel_enabled: bool,
}

impl Default for CommandBarState {
    fn default() -> Self {
        Self {
            back_visible: false,
            primary_enabled: true,
            cancel_enabled: true,
        }
    }
}

/// Owns presentation routing separately from the workflow state.
///
/// `navigate` never replaces `workflow`, which lets later PE parts migrate one complete page at a
/// time without restarting the install/backup/expand worker or losing its receiver/checkpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeWindowState<W> {
    pub page: NativePage,
    pub command_bar: CommandBarState,
    pub workflow: W,
}

impl<W> NativeWindowState<W> {
    pub fn new(workflow: W) -> Self {
        Self {
            page: NativePage::Overview,
            command_bar: CommandBarState::default(),
            workflow,
        }
    }

    pub fn navigate(&mut self, page: NativePage) {
        self.page = page;
    }

    pub fn into_workflow(self) -> W {
        self.workflow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_navigation_preserves_owned_workflow_state() {
        let mut state = NativeWindowState::new(String::from("worker-receiver-token"));
        state.navigate(NativePage::Progress);
        assert_eq!(state.page, NativePage::Progress);
        assert_eq!(state.into_workflow(), "worker-receiver-token");
    }

    #[test]
    fn p4_detail_pages_are_explicit_routes_without_replacing_workflow() {
        let mut state = NativeWindowState::new(42_u32);
        for page in [
            NativePage::AdvancedOptions,
            NativePage::Error,
            NativePage::Recovery,
            NativePage::Progress,
        ] {
            state.navigate(page);
            assert_eq!(state.page, page);
            assert_eq!(state.workflow, 42);
        }
    }
}
