use crate::preview::{CancelToken, Generation, Preview, PreviewError};
use std::path::PathBuf;

#[derive(Debug, PartialEq, Eq)]
pub enum PreviewState {
    Hidden,
    Loading {
        gen: Generation,
        path: PathBuf,
    },
    Showing {
        gen: Generation,
        path: PathBuf,
        preview: Preview,
    },
    Failed {
        gen: Generation,
        path: PathBuf,
        error: PreviewError,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    SpacePressed(PathBuf),
    EscPressed,
    LoadFinished(Generation, Result<Preview, PreviewError>),
    FocusLost,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    StartLoad {
        gen: Generation,
        path: PathBuf,
    },
    ShowWindow,
    /// Hide the preview. `restore_focus` returns focus to the Explorer window
    /// that opened it (Space and Esc). Focus loss passes false so the window
    /// the user switched to stays in front.
    HideWindow {
        restore_focus: bool,
    },
    CancelLoad,
}

pub fn step(
    state: PreviewState,
    event: Event,
    cancel: &CancelToken,
) -> (PreviewState, Vec<Effect>) {
    match (&state, event) {
        (PreviewState::Hidden, Event::SpacePressed(path)) => {
            let gen = cancel.generate();
            (
                PreviewState::Loading {
                    gen,
                    path: path.clone(),
                },
                vec![Effect::StartLoad { gen, path }],
            )
        }
        (
            PreviewState::Loading { .. }
            | PreviewState::Showing { .. }
            | PreviewState::Failed { .. },
            Event::SpacePressed(_),
        ) => (
            PreviewState::Hidden,
            vec![
                Effect::HideWindow {
                    restore_focus: true,
                },
                Effect::CancelLoad,
            ],
        ),
        (PreviewState::Showing { .. }, Event::EscPressed)
        | (PreviewState::Failed { .. }, Event::EscPressed) => (
            PreviewState::Hidden,
            vec![Effect::HideWindow {
                restore_focus: true,
            }],
        ),
        (PreviewState::Hidden, Event::EscPressed) => (PreviewState::Hidden, vec![]),
        (PreviewState::Loading { .. }, Event::EscPressed) => {
            (PreviewState::Hidden, vec![Effect::CancelLoad])
        }
        (_, Event::FocusLost) => {
            let cancel_effect = match &state {
                PreviewState::Hidden => None,
                PreviewState::Loading { .. } => Some(Effect::CancelLoad),
                PreviewState::Showing { .. } | PreviewState::Failed { .. } => {
                    Some(Effect::HideWindow {
                        restore_focus: false,
                    })
                }
            };
            match cancel_effect {
                None => (PreviewState::Hidden, vec![]),
                Some(e) => (PreviewState::Hidden, vec![e]),
            }
        }
        (_, Event::LoadFinished(gen, result)) => {
            if cancel.is_cancelled(&gen) {
                return (PreviewState::Hidden, vec![]);
            }
            if let PreviewState::Loading { gen: cur, path } = &state {
                if *cur == gen {
                    return match result {
                        Ok(preview) => (
                            PreviewState::Showing {
                                gen,
                                path: path.clone(),
                                preview,
                            },
                            vec![Effect::ShowWindow],
                        ),
                        Err(error) => (
                            PreviewState::Failed {
                                gen,
                                path: path.clone(),
                                error,
                            },
                            vec![Effect::ShowWindow],
                        ),
                    };
                }
            }
            (state, vec![])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path() -> PathBuf {
        PathBuf::from("C:\\test\\file.txt")
    }

    fn sample_preview() -> Preview {
        Preview {
            content: "hi".to_string(),
            truncated: false,
            line_count: 1,
        }
    }

    fn showing() -> PreviewState {
        PreviewState::Showing {
            gen: Generation::new(1),
            path: path(),
            preview: sample_preview(),
        }
    }

    #[test]
    fn space_from_hidden_starts_load() {
        let cancel = CancelToken::new();
        let (state, effects) = step(PreviewState::Hidden, Event::SpacePressed(path()), &cancel);
        let expected_gen = Generation::new(0);
        assert_eq!(
            state,
            PreviewState::Loading {
                gen: expected_gen,
                path: path(),
            }
        );
        assert_eq!(
            effects,
            vec![Effect::StartLoad {
                gen: expected_gen,
                path: path(),
            }]
        );
    }

    #[test]
    fn space_from_showing_toggles_to_hidden() {
        let cancel = CancelToken::new();
        let (state, effects) = step(showing(), Event::SpacePressed(path()), &cancel);
        assert_eq!(state, PreviewState::Hidden);
        assert_eq!(
            effects,
            vec![
                Effect::HideWindow {
                    restore_focus: true
                },
                Effect::CancelLoad,
            ]
        );
    }

    #[test]
    fn space_from_loading_hides_and_restores_focus() {
        let cancel = CancelToken::new();
        let state = PreviewState::Loading {
            gen: Generation::new(1),
            path: path(),
        };
        let (state, effects) = step(state, Event::SpacePressed(path()), &cancel);
        assert_eq!(state, PreviewState::Hidden);
        assert_eq!(
            effects,
            vec![
                Effect::HideWindow {
                    restore_focus: true
                },
                Effect::CancelLoad,
            ]
        );
    }

    #[test]
    fn esc_from_showing_hides_and_restores_focus() {
        let cancel = CancelToken::new();
        let (state, effects) = step(showing(), Event::EscPressed, &cancel);
        assert_eq!(state, PreviewState::Hidden);
        assert_eq!(
            effects,
            vec![Effect::HideWindow {
                restore_focus: true
            }]
        );
    }

    #[test]
    fn esc_from_failed_restores_focus() {
        let cancel = CancelToken::new();
        let state = PreviewState::Failed {
            gen: Generation::new(1),
            path: path(),
            error: PreviewError::Unsupported,
        };
        let (state, effects) = step(state, Event::EscPressed, &cancel);
        assert_eq!(state, PreviewState::Hidden);
        assert_eq!(
            effects,
            vec![Effect::HideWindow {
                restore_focus: true
            }]
        );
    }

    #[test]
    fn esc_from_loading_cancels() {
        let cancel = CancelToken::new();
        let state = PreviewState::Loading {
            gen: Generation::new(1),
            path: path(),
        };
        let (state, effects) = step(state, Event::EscPressed, &cancel);
        assert_eq!(state, PreviewState::Hidden);
        assert_eq!(effects, vec![Effect::CancelLoad]);
    }

    #[test]
    fn focus_lost_hides_showing_without_restoring_focus() {
        let cancel = CancelToken::new();
        let (state, effects) = step(showing(), Event::FocusLost, &cancel);
        assert_eq!(state, PreviewState::Hidden);
        assert_eq!(
            effects,
            vec![Effect::HideWindow {
                restore_focus: false
            }]
        );
    }

    #[test]
    fn focus_lost_from_failed_does_not_restore_focus() {
        let cancel = CancelToken::new();
        let state = PreviewState::Failed {
            gen: Generation::new(1),
            path: path(),
            error: PreviewError::Unsupported,
        };
        let (state, effects) = step(state, Event::FocusLost, &cancel);
        assert_eq!(state, PreviewState::Hidden);
        assert_eq!(
            effects,
            vec![Effect::HideWindow {
                restore_focus: false
            }]
        );
    }

    #[test]
    fn focus_lost_from_loading_cancels_without_hiding() {
        let cancel = CancelToken::new();
        let state = PreviewState::Loading {
            gen: Generation::new(1),
            path: path(),
        };
        let (state, effects) = step(state, Event::FocusLost, &cancel);
        assert_eq!(state, PreviewState::Hidden);
        assert_eq!(effects, vec![Effect::CancelLoad]);
    }

    #[test]
    fn stale_load_finished_is_ignored() {
        let cancel = CancelToken::new();
        let state = PreviewState::Loading {
            gen: Generation::new(5),
            path: path(),
        };
        let (state, effects) = step(
            state,
            Event::LoadFinished(Generation::new(9), Ok(sample_preview())),
            &cancel,
        );
        assert_eq!(
            state,
            PreviewState::Loading {
                gen: Generation::new(5),
                path: path(),
            }
        );
        assert!(effects.is_empty());
    }

    #[test]
    fn fresh_load_finished_shows() {
        let cancel = CancelToken::new();
        let (state, effects) = step(
            PreviewState::Loading {
                gen: Generation::new(5),
                path: path(),
            },
            Event::LoadFinished(Generation::new(5), Ok(sample_preview())),
            &cancel,
        );
        assert_eq!(
            state,
            PreviewState::Showing {
                gen: Generation::new(5),
                path: path(),
                preview: sample_preview(),
            }
        );
        assert_eq!(effects, vec![Effect::ShowWindow]);
    }

    #[test]
    fn failed_state_space_toggles_to_hidden() {
        let cancel = CancelToken::new();
        let state = PreviewState::Failed {
            gen: Generation::new(1),
            path: path(),
            error: PreviewError::Unsupported,
        };
        let (state, effects) = step(state, Event::SpacePressed(path()), &cancel);
        assert_eq!(state, PreviewState::Hidden);
        assert_eq!(
            effects,
            vec![
                Effect::HideWindow {
                    restore_focus: true
                },
                Effect::CancelLoad,
            ]
        );
    }
}
