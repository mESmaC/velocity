//! Pure keyboard dispatch for SPEC.md F1 and §5. Rendering and feature execution
//! consume intents at the application edge; typing never launches a bare action.

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigationView {
    Home,
    Thread,
    Duty,
    Swarm,
    SpecScan,
    SpecAsk,
    SpecReady,
    SpecLocked,
    Agents,
    Settings,
    Other,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigationIntent {
    Palette,
    NewThread,
    NewProject,
    Settings,
    ShowNumbers,
    Jump(
        #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
        #[cfg_attr(feature = "bindings", specta(type = String))]
        usize,
    ),
    Search,
    Task,
    Swarm,
    Goal,
    Airc,
    Dismiss,
    Submit,
    Dispatch,
    Resume,
    Fanout,
    PrDuty,
    Scouts,
    Answer(
        #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
        #[cfg_attr(feature = "bindings", specta(type = String))]
        usize,
    ),
    Lock,
    SpecFile,
    Grill,
    Diff,
    Editor,
    SaveAgents,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutKey {
    K,
    N,
    Comma,
    Alt,
    Slash,
    T,
    W,
    G,
    A,
    Escape,
    Enter,
    I,
    R,
    F,
    V,
    S,
    L,
    D,
    E,
    Digit(
        #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
        #[cfg_attr(feature = "bindings", specta(type = String))]
        usize,
    ),
    Other,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyStroke {
    pub key: ShortcutKey,
    pub control: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta_held: bool,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShortcutContext {
    pub view: NavigationView,
    pub typing: bool,
    pub overlay_open: bool,
    pub form_open: bool,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumberHintEvent {
    AltDown,
    AltUp,
    Blur,
    Other,
}

pub fn update_number_hints(shown: bool, event: NumberHintEvent) -> bool {
    match event {
        NumberHintEvent::AltDown => true,
        NumberHintEvent::AltUp | NumberHintEvent::Blur => false,
        NumberHintEvent::Other => shown,
    }
}

/// Resolve the overlapping S/G keys from the selected interview phase first.
pub fn bare_key_intent(view: NavigationView, key: ShortcutKey) -> Option<NavigationIntent> {
    use NavigationIntent as Intent;
    use ShortcutKey as Key;
    match key {
        Key::Slash => Some(Intent::Search),
        Key::T => Some(Intent::Task),
        Key::W => Some(Intent::Swarm),
        Key::G => Some(match view {
            NavigationView::SpecReady => Intent::Grill,
            _ => Intent::Goal,
        }),
        Key::A => Some(Intent::Airc),
        Key::I => Some(Intent::Dispatch),
        Key::R => Some(Intent::Resume),
        Key::F => Some(Intent::Fanout),
        Key::V => Some(Intent::PrDuty),
        Key::S => Some(match view {
            NavigationView::SpecReady => Intent::SpecFile,
            _ => Intent::Scouts,
        }),
        Key::L => Some(Intent::Lock),
        Key::D => Some(Intent::Diff),
        Key::E => Some(Intent::Editor),
        Key::Digit(number) => Some(Intent::Answer(number)),
        _ => None,
    }
}

pub fn decode_key(view: NavigationView, stroke: KeyStroke) -> Option<NavigationIntent> {
    if matches!(stroke.key, ShortcutKey::Escape) {
        Some(NavigationIntent::Dismiss)
    } else if stroke.meta_held {
        None
    } else if stroke.control {
        control_key_intent(stroke)
    } else if stroke.alt {
        match stroke.key {
            ShortcutKey::Alt => Some(NavigationIntent::ShowNumbers),
            ShortcutKey::Digit(number) => Some(NavigationIntent::Jump(number)),
            _ => None,
        }
    } else {
        bare_key_intent(view, stroke.key)
    }
}

fn control_key_intent(stroke: KeyStroke) -> Option<NavigationIntent> {
    use NavigationIntent as Intent;
    match stroke.key {
        ShortcutKey::K => Some(Intent::Palette),
        ShortcutKey::N => Some(if stroke.shift {
            Intent::NewProject
        } else {
            Intent::NewThread
        }),
        ShortcutKey::Comma => Some(Intent::Settings),
        ShortcutKey::Enter => Some(Intent::Submit),
        ShortcutKey::S => Some(Intent::SaveAgents),
        _ => None,
    }
}

pub fn route_bare_intent(
    view: NavigationView,
    intent: NavigationIntent,
) -> Option<NavigationIntent> {
    use NavigationIntent as Intent;
    match intent {
        Intent::Search | Intent::Task | Intent::Swarm | Intent::Airc => Some(intent),
        Intent::Goal => match view {
            NavigationView::SpecScan
            | NavigationView::SpecAsk
            | NavigationView::SpecReady
            | NavigationView::SpecLocked => None,
            _ => Some(intent),
        },
        Intent::Dispatch | Intent::Resume | Intent::Fanout | Intent::PrDuty | Intent::Scouts => {
            select_if(matches!(view, NavigationView::Home), intent)
        }
        Intent::Answer(number) => select_if(
            matches!(view, NavigationView::SpecAsk) && matches!(number, 1..=4),
            intent,
        ),
        Intent::Lock | Intent::SpecFile | Intent::Grill => {
            select_if(matches!(view, NavigationView::SpecReady), intent)
        }
        Intent::Diff | Intent::Editor => select_if(matches!(view, NavigationView::Thread), intent),
        _ => None,
    }
}

pub fn route_intent(
    context: ShortcutContext,
    control_chord: bool,
    intent: NavigationIntent,
) -> Option<NavigationIntent> {
    use NavigationIntent as Intent;
    match intent {
        Intent::Dismiss => Some(Intent::Dismiss),
        Intent::Palette
        | Intent::NewThread
        | Intent::NewProject
        | Intent::Settings
        | Intent::ShowNumbers => select_if(control_chord, intent),
        Intent::Jump(number) => select_if(control_chord && matches!(number, 1..=9), intent),
        Intent::Submit => select_if(control_chord && context.form_open, intent),
        Intent::SaveAgents => select_if(
            control_chord && matches!(context.view, NavigationView::Agents),
            intent,
        ),
        _ => {
            if context.typing || context.overlay_open || control_chord {
                None
            } else {
                route_bare_intent(context.view, intent)
            }
        }
    }
}

// Keep this branch in the extracted crate instead of relying on an unmodelled
// standard-library bool::then_some call in the confidence proof.
fn select_if(condition: bool, intent: NavigationIntent) -> Option<NavigationIntent> {
    if condition { Some(intent) } else { None }
}

pub fn shortcut_intent(context: ShortcutContext, stroke: KeyStroke) -> Option<NavigationIntent> {
    match decode_key(context.view, stroke) {
        Some(intent) => route_intent(context, stroke.control || stroke.alt, intent),
        None => None,
    }
}
