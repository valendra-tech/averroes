use gpui::actions;

actions!(
    averroes,
    [
        Quit,
        NewWindow,
        OpenWorkspace,
        SendMessage,
        NewSession,
        CloseSession,
        ToggleSettings,
        FocusInput,
    ]
);

#[derive(Clone, Debug, PartialEq, Eq, gpui::Action)]
#[action(namespace = averroes, no_json)]
pub struct OpenRecentProject {
    pub project_id: String,
}
