//! Which of an agent's `authMethods` Velocity signs in with (F15). Every provider signs in
//! with OAuth on your own subscription; Velocity never takes an API key, so methods that bill
//! an API account or route through a key-holding gateway are never chosen.

/// How a method runs, from its ACP shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignInKind {
    /// `authenticate { methodId }`: the agent runs the login itself.
    Agent,
    /// The client runs the agent's own program with these arguments, which opens the
    /// provider's sign-in page in your browser.
    Terminal { args: Vec<String> },
}

/// One advertised method.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignInMethod {
    pub id: String,
    pub name: String,
    pub description: String,
    pub kind: SignInKind,
}

/// Words that mark a method as an API-key or API-billing login.
const KEY_MARKERS: [&str; 7] = [
    "apikey",
    "api key",
    "console",
    "api usage",
    "gateway",
    "bedrock",
    "vertex",
];

/// Words that mark a subscription OAuth login, preferred over a generic one.
const OAUTH_MARKERS: [&str; 8] = [
    "subscription",
    "oauth",
    "chatgpt",
    "claude.ai",
    "claudeai",
    "google",
    "browser",
    "login",
];

/// The index of the method to sign in with: the first subscription OAuth method, else the
/// first method that is not an API-key login. `None` when every method takes a key.
pub fn choose_sign_in(methods: &[SignInMethod]) -> Option<usize> {
    let allowed: Vec<usize> = (0..methods.len())
        .filter(|&index| !mentions(&methods[index], &KEY_MARKERS))
        .collect();
    allowed
        .iter()
        .copied()
        .find(|&index| mentions(&methods[index], &OAUTH_MARKERS))
        .or_else(|| allowed.first().copied())
}

fn mentions(method: &SignInMethod, markers: &[&str]) -> bool {
    let mut text = format!("{} {} {}", method.id, method.name, method.description);
    if let SignInKind::Terminal { args } = &method.kind {
        text.push(' ');
        text.push_str(&args.join(" "));
    }
    let text = text.to_lowercase().replace(['-', '_'], " ");
    let squashed = text.replace(' ', "");
    markers
        .iter()
        .any(|marker| text.contains(marker) || squashed.contains(&marker.replace(' ', "")))
}
