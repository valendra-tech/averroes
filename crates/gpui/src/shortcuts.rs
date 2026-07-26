use gpui::actions;

actions!(
    averroes,
    [
        Quit,
        SendMessage,
        NewSession,
        CloseSession,
        ToggleSettings,
        FocusInput,
    ]
);
