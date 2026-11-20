use crate::keys::SessionPhase;
use crate::pdf::PdfError;
use crate::preview::{CancelToken, Generation, Preview, PreviewError, QuickInfo};
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub enum PreviewState {
    Hidden,
    /// `replacing` is true when a window is already on screen and a new file
    /// is loading into it. Esc then dismisses that window. When false, nothing
    /// has been shown yet and Esc only cancels the load.
    Loading {
        gen: Generation,
        path: PathBuf,
        replacing: bool,
    },
    /// The bytes are ready and the window is not visible. Esc, focus loss, and
    /// a failed show-gate discard this instead of showing it.
    PendingShow {
        gen: Generation,
        path: PathBuf,
        outcome: ShowOutcome,
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

/// Result waiting to be shown. Kept out of [`PreviewState::Showing`] until the
/// UI has confirmed the origin Explorer is still in front.
#[derive(Debug, PartialEq, Eq)]
pub enum ShowOutcome {
    Ready(Preview),
    Failed(PreviewError),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    SpacePressed(PathBuf),
    EscPressed,
    LoadFinished(Generation, Result<Preview, PreviewError>),
    /// The PDF worker finished a first page for this generation.
    ///
    /// Applied only while the shell-icon placeholder for that generation is
    /// still the current preview. A stale, cancelled, or closed result is
    /// dropped, including one that arrives after Esc.
    PdfReady(Generation, Result<Preview, PreviewError>),
    FocusLost,
    /// The window was shown with no activation. Only [`PreviewState::PendingShow`].
    ShowCommitted,
    /// The window must not appear. Only [`PreviewState::PendingShow`].
    ShowAborted,
    /// The Explorer selection moved while a session is active.
    SelectionChanged(PathBuf),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    StartLoad {
        gen: Generation,
        path: PathBuf,
    },
    /// Fill a hidden window, then show it only if the origin is still in front.
    PresentPreview,
    /// Replace the text of a window that is already on screen.
    UpdateContent,
    /// Hide the preview. `restore_focus` returns focus to the Explorer window
    /// that opened it (Space and Esc on a visible window). Focus loss passes
    /// false so the window the user switched to stays in front.
    HideWindow {
        restore_focus: bool,
    },
    CancelLoad,
}

/// Whether the keyboard hook should treat the session as not yet visible.
pub fn session_phase(state: &PreviewState) -> SessionPhase {
    match state {
        PreviewState::Hidden => SessionPhase::Hidden,
        PreviewState::Loading {
            replacing: false, ..
        }
        | PreviewState::PendingShow { .. } => SessionPhase::Pending,
        PreviewState::Loading {
            replacing: true, ..
        }
        | PreviewState::Showing { .. }
        | PreviewState::Failed { .. } => SessionPhase::Visible,
    }
}

/// Last chance before a not-yet-visible preview is put on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShowGate {
    Show,
    Discard,
}

pub fn gate_show(origin_alive: bool, origin_is_foreground: bool, dismiss_queued: bool) -> ShowGate {
    if origin_alive && origin_is_foreground && !dismiss_queued {
        ShowGate::Show
    } else {
        ShowGate::Discard
    }
}

pub fn step(
    state: PreviewState,
    event: Event,
    cancel: &CancelToken,
) -> (PreviewState, Vec<Effect>) {
    match (state, event) {
        (PreviewState::Hidden, Event::SpacePressed(path)) if path.as_os_str().is_empty() => {
            // No filesystem path. Do not start a load and do not show a window.
            (PreviewState::Hidden, vec![])
        }
        (PreviewState::Hidden, Event::SpacePressed(path)) => {
            let gen = cancel.generate();
            (
                PreviewState::Loading {
                    gen,
                    path: path.clone(),
                    replacing: false,
                },
                vec![Effect::StartLoad { gen, path }],
            )
        }
        (
            PreviewState::Loading { .. }
            | PreviewState::PendingShow { .. }
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
        (
            PreviewState::Loading {
                replacing: false, ..
            },
            Event::EscPressed,
        ) => (PreviewState::Hidden, vec![Effect::CancelLoad]),
        (
            PreviewState::Loading {
                replacing: true, ..
            },
            Event::EscPressed,
        ) => (
            PreviewState::Hidden,
            vec![
                Effect::HideWindow {
                    restore_focus: true,
                },
                Effect::CancelLoad,
            ],
        ),
        (PreviewState::PendingShow { .. }, Event::EscPressed) => (
            PreviewState::Hidden,
            vec![
                Effect::HideWindow {
                    restore_focus: false,
                },
                Effect::CancelLoad,
            ],
        ),
        // The icon is already on screen and a render may still be in flight.
        // Esc has to cancel that render, not only hide the window.
        (PreviewState::Failed { error, .. }, Event::EscPressed) if error.is_pdf_placeholder() => (
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
        (PreviewState::Hidden, Event::FocusLost) => (PreviewState::Hidden, vec![]),
        (
            PreviewState::Loading {
                replacing: false, ..
            },
            Event::FocusLost,
        ) => (PreviewState::Hidden, vec![Effect::CancelLoad]),
        (
            PreviewState::Loading {
                replacing: true, ..
            },
            Event::FocusLost,
        ) => (
            PreviewState::Hidden,
            vec![
                Effect::HideWindow {
                    restore_focus: false,
                },
                Effect::CancelLoad,
            ],
        ),
        (PreviewState::PendingShow { .. }, Event::FocusLost | Event::ShowAborted) => (
            PreviewState::Hidden,
            vec![
                Effect::HideWindow {
                    restore_focus: false,
                },
                Effect::CancelLoad,
            ],
        ),
        (PreviewState::Failed { error, .. }, Event::FocusLost) if error.is_pdf_placeholder() => (
            PreviewState::Hidden,
            vec![
                Effect::HideWindow {
                    restore_focus: false,
                },
                Effect::CancelLoad,
            ],
        ),
        (PreviewState::Showing { .. } | PreviewState::Failed { .. }, Event::FocusLost) => (
            PreviewState::Hidden,
            vec![Effect::HideWindow {
                restore_focus: false,
            }],
        ),
        (PreviewState::PendingShow { gen, path, outcome }, Event::ShowCommitted) => match outcome {
            ShowOutcome::Ready(preview) => (PreviewState::Showing { gen, path, preview }, vec![]),
            ShowOutcome::Failed(error) => (PreviewState::Failed { gen, path, error }, vec![]),
        },
        (state, Event::ShowCommitted | Event::ShowAborted) => (state, vec![]),
        (state, Event::SelectionChanged(path)) => selection_changed(state, path, cancel),
        (state, Event::LoadFinished(gen, result)) => load_finished(state, gen, result, cancel),
        (state, Event::PdfReady(gen, result)) => pdf_ready(state, gen, result, cancel),
    }
}

fn selection_changed(
    state: PreviewState,
    new_path: PathBuf,
    cancel: &CancelToken,
) -> (PreviewState, Vec<Effect>) {
    // A cleared selection, or an item with no filesystem path, is not a file
    // to load. Keep whatever is already on screen.
    if new_path.as_os_str().is_empty() {
        return (state, vec![]);
    }
    let Some(current) = active_path(&state) else {
        return (state, vec![]);
    };
    if current == new_path.as_path() {
        return (state, vec![]);
    }
    let replacing = matches!(
        state,
        PreviewState::Loading {
            replacing: true,
            ..
        } | PreviewState::Showing { .. }
            | PreviewState::Failed { .. }
    );
    let gen = cancel.generate();
    let path = new_path;
    let next = PreviewState::Loading {
        gen,
        path: path.clone(),
        replacing,
    };
    // A window that was never shown must be destroyed so its fill cannot appear
    // after the selection has already moved on.
    let effects = if replacing {
        vec![Effect::StartLoad { gen, path }]
    } else {
        vec![
            Effect::HideWindow {
                restore_focus: false,
            },
            Effect::StartLoad { gen, path },
        ]
    };
    (next, effects)
}

fn active_path(state: &PreviewState) -> Option<&Path> {
    match state {
        PreviewState::Hidden => None,
        PreviewState::Loading { path, .. }
        | PreviewState::PendingShow { path, .. }
        | PreviewState::Showing { path, .. }
        | PreviewState::Failed { path, .. } => Some(path),
    }
}

fn load_finished(
    state: PreviewState,
    gen: Generation,
    result: Result<Preview, PreviewError>,
    cancel: &CancelToken,
) -> (PreviewState, Vec<Effect>) {
    // A cancelled generation must not clobber a newer session.
    if cancel.is_cancelled(&gen) {
        return (state, vec![]);
    }
    let PreviewState::Loading {
        gen: cur,
        path,
        replacing,
    } = state
    else {
        return (state, vec![]);
    };
    if cur != gen {
        return (
            PreviewState::Loading {
                gen: cur,
                path,
                replacing,
            },
            vec![],
        );
    }
    if replacing {
        return match result {
            Ok(preview) => (
                PreviewState::Showing { gen, path, preview },
                vec![Effect::UpdateContent],
            ),
            Err(error) => (
                PreviewState::Failed { gen, path, error },
                vec![Effect::UpdateContent],
            ),
        };
    }
    let outcome = match result {
        Ok(preview) => ShowOutcome::Ready(preview),
        Err(error) => ShowOutcome::Failed(error),
    };
    (
        PreviewState::PendingShow { gen, path, outcome },
        vec![Effect::PresentPreview],
    )
}

/// Replace the shell-icon placeholder with the page, or with a failure that
/// keeps the placeholder's quick info. Anything else is ignored so a late
/// bitmap cannot paint over a newer file, a closed window, or a failure
/// that already replaced the placeholder.
fn pdf_ready(
    state: PreviewState,
    gen: Generation,
    result: Result<Preview, PreviewError>,
    cancel: &CancelToken,
) -> (PreviewState, Vec<Effect>) {
    if cancel.is_cancelled(&gen) || pdf_result_dropped(&result) {
        return (state, vec![]);
    }
    match state {
        PreviewState::Failed {
            gen: cur,
            path,
            error,
        } if cur == gen && error.is_pdf_placeholder() => match result {
            Ok(preview) => (
                PreviewState::Showing { gen, path, preview },
                vec![Effect::UpdateContent],
            ),
            Err(incoming) => (
                PreviewState::Failed {
                    gen,
                    path,
                    error: incoming.with_quick_info(quick_info_or_default(&error)),
                },
                vec![Effect::UpdateContent],
            ),
        },
        PreviewState::PendingShow {
            gen: cur,
            path,
            outcome: ShowOutcome::Failed(error),
        } if cur == gen && error.is_pdf_placeholder() => {
            let info = quick_info_or_default(&error);
            let outcome = match result {
                Ok(preview) => ShowOutcome::Ready(preview),
                Err(incoming) => ShowOutcome::Failed(incoming.with_quick_info(info)),
            };
            (PreviewState::PendingShow { gen, path, outcome }, vec![])
        }
        other => (other, vec![]),
    }
}

fn pdf_result_dropped(result: &Result<Preview, PreviewError>) -> bool {
    matches!(
        result,
        Err(PreviewError::Cancelled)
            | Err(PreviewError::Pdf {
                error: PdfError::Cancelled,
                ..
            })
    )
}

fn quick_info_or_default(error: &PreviewError) -> QuickInfo {
    error.quick_info().cloned().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path() -> PathBuf {
        PathBuf::from("file.txt")
    }

    fn other_path() -> PathBuf {
        PathBuf::from("other.txt")
    }

    fn sample_preview() -> Preview {
        Preview {
            content: "hi".to_string(),
            truncated: false,
            line_count: 1,
            truncation: None,
            total_bytes: 2,
            image: None,
            markdown_rtf: None,
            pdf_pages: 0,
        }
    }

    fn showing() -> PreviewState {
        PreviewState::Showing {
            gen: Generation::new(1),
            path: path(),
            preview: sample_preview(),
        }
    }

    fn loading(replacing: bool) -> PreviewState {
        PreviewState::Loading {
            gen: Generation::new(1),
            path: path(),
            replacing,
        }
    }

    #[test]
    fn space_with_no_path_does_not_open_a_window() {
        let cancel = CancelToken::new();
        let (state, effects) = step(
            PreviewState::Hidden,
            Event::SpacePressed(PathBuf::new()),
            &cancel,
        );
        assert_eq!(state, PreviewState::Hidden);
        assert!(effects.is_empty());
        assert_eq!(session_phase(&state), SessionPhase::Hidden);
    }

    #[test]
    fn selection_cleared_keeps_the_current_preview() {
        let cancel = CancelToken::new();
        let (state, effects) = step(showing(), Event::SelectionChanged(PathBuf::new()), &cancel);
        assert_eq!(state, showing());
        assert!(effects.is_empty());
        let (state, effects) = step(
            PreviewState::Hidden,
            Event::SelectionChanged(PathBuf::new()),
            &cancel,
        );
        assert_eq!(state, PreviewState::Hidden);
        assert!(effects.is_empty());
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
                replacing: false,
            }
        );
        assert_eq!(
            effects,
            vec![Effect::StartLoad {
                gen: expected_gen,
                path: path(),
            }]
        );
        assert_eq!(session_phase(&state), SessionPhase::Pending);
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
        let (state, effects) = step(loading(false), Event::SpacePressed(path()), &cancel);
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
            error: PreviewError::binary(),
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
        let (state, effects) = step(loading(false), Event::EscPressed, &cancel);
        assert_eq!(state, PreviewState::Hidden);
        assert_eq!(effects, vec![Effect::CancelLoad]);
    }

    #[test]
    fn esc_while_replacing_hides_the_visible_window() {
        let cancel = CancelToken::new();
        let (state, effects) = step(loading(true), Event::EscPressed, &cancel);
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
            error: PreviewError::binary(),
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
        let (state, effects) = step(loading(false), Event::FocusLost, &cancel);
        assert_eq!(state, PreviewState::Hidden);
        assert_eq!(effects, vec![Effect::CancelLoad]);
    }

    #[test]
    fn focus_lost_while_replacing_hides_without_restoring_focus() {
        let cancel = CancelToken::new();
        let (state, effects) = step(loading(true), Event::FocusLost, &cancel);
        assert_eq!(state, PreviewState::Hidden);
        assert_eq!(
            effects,
            vec![
                Effect::HideWindow {
                    restore_focus: false
                },
                Effect::CancelLoad,
            ]
        );
    }

    #[test]
    fn stale_load_finished_is_ignored() {
        let cancel = CancelToken::new();
        let (state, effects) = step(
            loading(false),
            Event::LoadFinished(Generation::new(9), Ok(sample_preview())),
            &cancel,
        );
        assert_eq!(state, loading(false));
        assert!(effects.is_empty());
    }

    #[test]
    fn fresh_load_finished_waits_for_commit_before_showing() {
        let cancel = CancelToken::new();
        let (state, effects) = step(
            PreviewState::Loading {
                gen: Generation::new(5),
                path: path(),
                replacing: false,
            },
            Event::LoadFinished(Generation::new(5), Ok(sample_preview())),
            &cancel,
        );
        assert_eq!(
            state,
            PreviewState::PendingShow {
                gen: Generation::new(5),
                path: path(),
                outcome: ShowOutcome::Ready(sample_preview()),
            }
        );
        assert_eq!(effects, vec![Effect::PresentPreview]);
        assert_eq!(session_phase(&state), SessionPhase::Pending);

        let (state, effects) = step(state, Event::ShowCommitted, &cancel);
        assert_eq!(
            state,
            PreviewState::Showing {
                gen: Generation::new(5),
                path: path(),
                preview: sample_preview(),
            }
        );
        assert!(effects.is_empty());
        assert_eq!(session_phase(&state), SessionPhase::Visible);
    }

    #[test]
    fn esc_while_pending_show_discards_before_the_window_appears() {
        let cancel = CancelToken::new();
        let (pending, _) = step(
            loading(false),
            Event::LoadFinished(Generation::new(1), Ok(sample_preview())),
            &cancel,
        );
        assert!(matches!(pending, PreviewState::PendingShow { .. }));
        let (state, effects) = step(pending, Event::EscPressed, &cancel);
        assert_eq!(state, PreviewState::Hidden);
        assert_eq!(
            effects,
            vec![
                Effect::HideWindow {
                    restore_focus: false
                },
                Effect::CancelLoad,
            ]
        );
        let (state, effects) = step(
            state,
            Event::LoadFinished(Generation::new(1), Ok(sample_preview())),
            &cancel,
        );
        assert_eq!(state, PreviewState::Hidden);
        assert!(effects.is_empty());
    }

    #[test]
    fn focus_lost_while_pending_discards() {
        let cancel = CancelToken::new();
        let (pending, _) = step(
            loading(false),
            Event::LoadFinished(Generation::new(1), Err(PreviewError::binary())),
            &cancel,
        );
        let (state, effects) = step(pending, Event::FocusLost, &cancel);
        assert_eq!(state, PreviewState::Hidden);
        assert_eq!(
            effects,
            vec![
                Effect::HideWindow {
                    restore_focus: false
                },
                Effect::CancelLoad,
            ]
        );
    }

    #[test]
    fn show_aborted_discards_pending_show() {
        let cancel = CancelToken::new();
        let (pending, effects) = step(
            loading(false),
            Event::LoadFinished(Generation::new(1), Ok(sample_preview())),
            &cancel,
        );
        assert_eq!(effects, vec![Effect::PresentPreview]);
        let (state, effects) = step(pending, Event::ShowAborted, &cancel);
        assert_eq!(state, PreviewState::Hidden);
        assert_eq!(
            effects,
            vec![
                Effect::HideWindow {
                    restore_focus: false
                },
                Effect::CancelLoad,
            ]
        );
        let (state, effects) = step(showing(), Event::ShowAborted, &cancel);
        assert_eq!(state, showing());
        assert!(effects.is_empty());
    }

    #[test]
    fn load_finished_after_cancel_does_not_clobber_a_newer_session() {
        let cancel = CancelToken::new();
        let stale = cancel.generate();
        let _live = cancel.generate();
        assert!(cancel.is_cancelled(&stale));
        let (state, effects) = step(
            showing(),
            Event::LoadFinished(stale, Ok(sample_preview())),
            &cancel,
        );
        assert_eq!(state, showing());
        assert!(effects.is_empty());
    }

    #[test]
    fn selection_change_reloads_without_hiding_a_visible_preview() {
        let cancel = CancelToken::new();
        let (state, _) = step(PreviewState::Hidden, Event::SpacePressed(path()), &cancel);
        let (state, _) = step(
            state,
            Event::LoadFinished(Generation::new(0), Ok(sample_preview())),
            &cancel,
        );
        let (state, _) = step(state, Event::ShowCommitted, &cancel);
        let (state, effects) = step(state, Event::SelectionChanged(other_path()), &cancel);
        let PreviewState::Loading {
            gen,
            path: loaded,
            replacing,
        } = &state
        else {
            panic!("expected a reload, got {state:?}");
        };
        assert!(*replacing);
        assert_eq!(loaded, &other_path());
        let gen = *gen;
        assert_eq!(
            effects,
            vec![Effect::StartLoad {
                gen,
                path: other_path(),
            }]
        );
        assert!(cancel.is_cancelled(&Generation::new(0)));
        assert_eq!(session_phase(&state), SessionPhase::Visible);

        let (state, effects) = step(
            state,
            Event::LoadFinished(gen, Ok(sample_preview())),
            &cancel,
        );
        assert!(matches!(state, PreviewState::Showing { .. }));
        assert_eq!(effects, vec![Effect::UpdateContent]);
    }

    #[test]
    fn selection_change_to_the_same_path_is_ignored() {
        let cancel = CancelToken::new();
        let (state, effects) = step(showing(), Event::SelectionChanged(path()), &cancel);
        assert_eq!(state, showing());
        assert!(effects.is_empty());
        let (state, effects) = step(
            PreviewState::Hidden,
            Event::SelectionChanged(path()),
            &cancel,
        );
        assert_eq!(state, PreviewState::Hidden);
        assert!(effects.is_empty());
    }

    #[test]
    fn unsupported_same_path_reload_does_not_start_a_new_load() {
        let cancel = CancelToken::new();
        let failed = PreviewState::Failed {
            gen: Generation::new(1),
            path: path(),
            error: PreviewError::binary(),
        };
        let (state, effects) = step(failed, Event::SelectionChanged(path()), &cancel);
        assert_eq!(
            state,
            PreviewState::Failed {
                gen: Generation::new(1),
                path: path(),
                error: PreviewError::binary(),
            }
        );
        assert!(effects.is_empty());
        assert!(!cancel.is_cancelled(&Generation::new(1)));
    }

    #[test]
    fn selection_change_during_load_cancels_the_previous_file() {
        let cancel = CancelToken::new();
        let (state, _) = step(PreviewState::Hidden, Event::SpacePressed(path()), &cancel);
        let first = Generation::new(0);
        let (state, effects) = step(state, Event::SelectionChanged(other_path()), &cancel);
        let PreviewState::Loading {
            gen,
            replacing,
            path: loaded,
        } = &state
        else {
            panic!("expected loading, got {state:?}");
        };
        assert!(!*replacing);
        assert_eq!(loaded, &other_path());
        assert!(cancel.is_cancelled(&first));
        assert_eq!(
            effects,
            vec![
                Effect::HideWindow {
                    restore_focus: false
                },
                Effect::StartLoad {
                    gen: *gen,
                    path: other_path(),
                },
            ]
        );
        let (state, effects) = step(
            state,
            Event::LoadFinished(first, Ok(sample_preview())),
            &cancel,
        );
        assert!(matches!(
            state,
            PreviewState::Loading {
                replacing: false,
                ..
            }
        ));
        assert!(effects.is_empty());
    }

    #[test]
    fn selection_change_while_pending_drops_the_unshown_window() {
        let cancel = CancelToken::new();
        let (state, _) = step(PreviewState::Hidden, Event::SpacePressed(path()), &cancel);
        let (pending, _) = step(
            state,
            Event::LoadFinished(Generation::new(0), Ok(sample_preview())),
            &cancel,
        );
        assert_eq!(session_phase(&pending), SessionPhase::Pending);
        let (state, effects) = step(pending, Event::SelectionChanged(other_path()), &cancel);
        assert!(matches!(
            state,
            PreviewState::Loading {
                replacing: false,
                ..
            }
        ));
        assert!(effects.iter().any(|effect| matches!(
            effect,
            Effect::HideWindow {
                restore_focus: false
            }
        )));
        assert!(effects
            .iter()
            .any(|effect| matches!(effect, Effect::StartLoad { .. })));
    }

    #[test]
    fn failed_state_space_toggles_to_hidden() {
        let cancel = CancelToken::new();
        let state = PreviewState::Failed {
            gen: Generation::new(1),
            path: path(),
            error: PreviewError::binary(),
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
    fn gate_show_requires_a_live_foreground_explorer() {
        assert_eq!(gate_show(true, true, false), ShowGate::Show);
        assert_eq!(gate_show(false, true, false), ShowGate::Discard);
        assert_eq!(gate_show(true, false, false), ShowGate::Discard);
        assert_eq!(gate_show(true, true, true), ShowGate::Discard);
        assert_eq!(gate_show(false, false, true), ShowGate::Discard);
    }

    #[test]
    fn session_phase_tracks_visibility() {
        assert_eq!(session_phase(&PreviewState::Hidden), SessionPhase::Hidden);
        assert_eq!(session_phase(&loading(false)), SessionPhase::Pending);
        assert_eq!(session_phase(&loading(true)), SessionPhase::Visible);
        assert_eq!(session_phase(&showing()), SessionPhase::Visible);
        assert_eq!(
            session_phase(&PreviewState::PendingShow {
                gen: Generation::new(1),
                path: path(),
                outcome: ShowOutcome::Failed(PreviewError::binary()),
            }),
            SessionPhase::Pending
        );
        assert_eq!(SessionPhase::from_code(0), SessionPhase::Hidden);
        assert_eq!(SessionPhase::from_code(1), SessionPhase::Pending);
        assert_eq!(SessionPhase::from_code(2), SessionPhase::Visible);
        assert_eq!(SessionPhase::from_code(9), SessionPhase::Hidden);
    }

    fn pdf_path() -> PathBuf {
        PathBuf::from("notes.pdf")
    }

    fn placeholder() -> PreviewError {
        PreviewError::pdf(PdfError::Rendering).with_quick_info(QuickInfo {
            type_name: Some("PDF Document".to_string()),
            size_bytes: Some(100),
            modified: Some("today".to_string()),
        })
    }

    fn failed_pdf(gen: u64) -> PreviewState {
        PreviewState::Failed {
            gen: Generation::new(gen),
            path: pdf_path(),
            error: placeholder(),
        }
    }

    fn page_preview() -> Preview {
        Preview {
            pdf_pages: 12,
            ..sample_preview()
        }
    }

    #[test]
    fn pdf_placeholder_shows_immediately_like_any_other_load_error() {
        let cancel = CancelToken::new();
        let (loading_state, _) = step(
            PreviewState::Hidden,
            Event::SpacePressed(pdf_path()),
            &cancel,
        );
        let (state, effects) = step(
            loading_state,
            Event::LoadFinished(Generation::new(0), Err(placeholder())),
            &cancel,
        );
        assert!(matches!(
            state,
            PreviewState::PendingShow {
                outcome: ShowOutcome::Failed(ref error),
                ..
            } if error.is_pdf_placeholder()
        ));
        assert_eq!(effects, vec![Effect::PresentPreview]);
        assert_eq!(session_phase(&state), SessionPhase::Pending);

        let (state, _) = step(state, Event::ShowCommitted, &cancel);
        assert!(matches!(
            state,
            PreviewState::Failed { ref error, .. } if error.is_pdf_placeholder()
        ));
        assert_eq!(session_phase(&state), SessionPhase::Visible);
    }

    #[test]
    fn a_matching_pdf_page_replaces_the_icon() {
        let cancel = CancelToken::new();
        let (state, effects) = step(
            failed_pdf(1),
            Event::PdfReady(Generation::new(1), Ok(page_preview())),
            &cancel,
        );
        assert_eq!(effects, vec![Effect::UpdateContent]);
        match state {
            PreviewState::Showing { preview, path, .. } => {
                assert_eq!(preview.pdf_pages, 12);
                assert_eq!(path, pdf_path());
            }
            other => panic!("expected the page, got {other:?}"),
        }
    }

    #[test]
    fn a_stale_pdf_page_does_not_replace_the_icon() {
        let cancel = CancelToken::new();
        let icon = failed_pdf(2);
        let (state, effects) = step(
            icon,
            Event::PdfReady(Generation::new(1), Ok(page_preview())),
            &cancel,
        );
        assert!(matches!(
            state,
            PreviewState::Failed { error, .. } if error.is_pdf_placeholder()
        ));
        assert!(effects.is_empty());
    }

    #[test]
    fn esc_cancels_the_pdf_render_and_drops_a_late_page() {
        let cancel = CancelToken::new();
        let gen = cancel.generate();
        let showing_icon = PreviewState::Failed {
            gen,
            path: pdf_path(),
            error: placeholder(),
        };
        let (state, effects) = step(showing_icon, Event::EscPressed, &cancel);
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
        // `apply` bumps the token when it runs CancelLoad. A finishing
        // render for the old generation must not reopen the window.
        let _ = cancel.generate();
        let (state, effects) = step(state, Event::PdfReady(gen, Ok(page_preview())), &cancel);
        assert_eq!(state, PreviewState::Hidden);
        assert!(effects.is_empty());
    }

    #[test]
    fn a_bumped_token_drops_a_page_while_the_icon_is_still_up() {
        let cancel = CancelToken::new();
        let gen = cancel.generate();
        let _newer = cancel.generate();
        let (state, effects) = step(
            PreviewState::Failed {
                gen,
                path: pdf_path(),
                error: placeholder(),
            },
            Event::PdfReady(gen, Ok(page_preview())),
            &cancel,
        );
        assert!(matches!(
            state,
            PreviewState::Failed { error, .. } if error.is_pdf_placeholder()
        ));
        assert!(effects.is_empty());
    }

    #[test]
    fn focus_lost_cancels_an_in_flight_pdf_render() {
        let cancel = CancelToken::new();
        let (state, effects) = step(failed_pdf(4), Event::FocusLost, &cancel);
        assert_eq!(state, PreviewState::Hidden);
        assert_eq!(
            effects,
            vec![
                Effect::HideWindow {
                    restore_focus: false
                },
                Effect::CancelLoad,
            ]
        );
    }

    #[test]
    fn a_new_file_drops_the_pdf_bitmap_and_a_late_page() {
        let cancel = CancelToken::new();
        let (state, _) = step(
            failed_pdf(1),
            Event::PdfReady(Generation::new(1), Ok(page_preview())),
            &cancel,
        );
        assert!(matches!(state, PreviewState::Showing { .. }));
        let (state, effects) = step(state, Event::SelectionChanged(other_path()), &cancel);
        assert!(matches!(
            state,
            PreviewState::Loading {
                replacing: true,
                ..
            }
        ));
        assert!(effects
            .iter()
            .any(|effect| matches!(effect, Effect::StartLoad { .. })));
        let (state, effects) = step(
            state,
            Event::PdfReady(Generation::new(1), Ok(page_preview())),
            &cancel,
        );
        assert!(matches!(state, PreviewState::Loading { .. }));
        assert!(effects.is_empty());
    }

    #[test]
    fn pdf_failure_keeps_the_placeholder_quick_info() {
        let cancel = CancelToken::new();
        let (state, effects) = step(
            failed_pdf(1),
            Event::PdfReady(
                Generation::new(1),
                Err(PreviewError::pdf(PdfError::PasswordProtected)),
            ),
            &cancel,
        );
        assert_eq!(effects, vec![Effect::UpdateContent]);
        match state {
            PreviewState::Failed { error, .. } => {
                assert!(matches!(
                    error,
                    PreviewError::Pdf {
                        error: PdfError::PasswordProtected,
                        ..
                    }
                ));
                let info = error.quick_info().unwrap();
                assert_eq!(info.type_name.as_deref(), Some("PDF Document"));
                assert_eq!(info.size_bytes, Some(100));
            }
            other => panic!("expected the failure view, got {other:?}"),
        }
    }

    #[test]
    fn a_cancelled_pdf_result_keeps_the_icon() {
        let cancel = CancelToken::new();
        for result in [
            Err(PreviewError::Cancelled),
            Err(PreviewError::pdf(PdfError::Cancelled)),
        ] {
            let (state, effects) = step(
                failed_pdf(1),
                Event::PdfReady(Generation::new(1), result),
                &cancel,
            );
            assert!(matches!(
                state,
                PreviewState::Failed { error, .. } if error.is_pdf_placeholder()
            ));
            assert!(effects.is_empty());
        }
    }

    #[test]
    fn a_pdf_page_that_arrives_before_show_replaces_the_pending_outcome() {
        let cancel = CancelToken::new();
        let pending = PreviewState::PendingShow {
            gen: Generation::new(3),
            path: pdf_path(),
            outcome: ShowOutcome::Failed(placeholder()),
        };
        let (state, effects) = step(
            pending,
            Event::PdfReady(Generation::new(3), Ok(page_preview())),
            &cancel,
        );
        assert!(effects.is_empty());
        match state {
            PreviewState::PendingShow {
                outcome: ShowOutcome::Ready(preview),
                ..
            } => assert_eq!(preview.pdf_pages, 12),
            other => panic!("expected a pending page, got {other:?}"),
        }
    }

    #[test]
    fn a_locked_pdf_keeps_the_in_use_sentence_and_drops_a_late_page() {
        let cancel = CancelToken::new();
        let (state, effects) = step(
            failed_pdf(4),
            Event::PdfReady(
                Generation::new(4),
                Err(crate::preview::preview_error_from_pdf(PdfError::Io(32))),
            ),
            &cancel,
        );
        assert_eq!(effects, vec![Effect::UpdateContent]);
        match &state {
            PreviewState::Failed { error, .. } => {
                assert_eq!(
                    error.user_message("notes.pdf"),
                    "notes.pdf is open in another program and can't be read"
                );
                assert!(!error.is_pdf());
                assert!(!error.is_pdf_placeholder());
            }
            other => panic!("expected the in-use failure, got {other:?}"),
        }
        let (state, effects) = step(
            state,
            Event::PdfReady(Generation::new(4), Ok(page_preview())),
            &cancel,
        );
        assert!(effects.is_empty());
        match state {
            PreviewState::Failed { error, .. } => {
                assert!(!error.is_pdf());
            }
            other => panic!("a late page painted over the in-use failure: {other:?}"),
        }
    }

    #[test]
    fn a_late_pdf_page_does_not_overwrite_a_failure() {
        let cancel = CancelToken::new();
        let (failed, _) = step(
            failed_pdf(1),
            Event::PdfReady(
                Generation::new(1),
                Err(PreviewError::pdf(PdfError::Damaged)),
            ),
            &cancel,
        );
        let (state, effects) = step(
            failed,
            Event::PdfReady(Generation::new(1), Ok(page_preview())),
            &cancel,
        );
        assert!(effects.is_empty());
        assert!(matches!(
            state,
            PreviewState::Failed {
                error: PreviewError::Pdf {
                    error: PdfError::Damaged,
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn non_pdf_esc_and_focus_loss_do_not_grow_a_cancel() {
        let cancel = CancelToken::new();
        let failed = PreviewState::Failed {
            gen: Generation::new(1),
            path: path(),
            error: PreviewError::binary(),
        };
        let (_, effects) = step(failed, Event::EscPressed, &cancel);
        assert_eq!(
            effects,
            vec![Effect::HideWindow {
                restore_focus: true
            }]
        );
        let failed = PreviewState::Failed {
            gen: Generation::new(1),
            path: path(),
            error: PreviewError::binary(),
        };
        let (_, effects) = step(failed, Event::FocusLost, &cancel);
        assert_eq!(
            effects,
            vec![Effect::HideWindow {
                restore_focus: false
            }]
        );
    }
}
