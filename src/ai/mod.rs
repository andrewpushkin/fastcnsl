//! The AI assistant: provider settings, API keys, and (in later modules)
//! the requests that ask a model to fix a command.

pub mod chat;
pub mod claude_code;
pub mod client;
pub mod context;
pub mod fix;
pub mod grok;
pub mod openrouter;
pub mod keys;
pub mod login;
pub mod wire;

use serde::{Deserialize, Serialize};

pub use keys::Keys;

/// An answer from a request thread, for the UI thread. `job` ties it to
/// the request: answers to requests since cancelled or replaced are dropped.
#[derive(Debug, Clone)]
pub enum AiEvent {
    /// Tool calls of the Fix `job` for the tab with id `tab`, to run on the
    /// UI thread; one result per call goes back on `reply`. Dropping
    /// `reply` unanswered ends the Fix.
    Tools { job: u64, tab: u64, calls: Vec<wire::ToolCall>, reply: std::sync::mpsc::Sender<Vec<String>> },
    /// The answer to a chat message in the tab with id `tab`.
    ChatTurn { job: u64, tab: u64, result: Result<chat::Turn, AiError> },
    /// Autopilot's work in the terminal of the tab with id `tab`; the
    /// answer goes back on `reply`. Dropping `reply` ends the turn.
    Drive { job: u64, tab: u64, request: chat::Drive, reply: std::sync::mpsc::Sender<chat::Driven> },
    /// A Fix for the tab with id `tab`.
    Fixed { job: u64, tab: u64, result: Result<fix::Fix, AiError> },
    /// The model list for Options → AI.
    Models { job: u64, result: Result<Vec<String>, AiError> },
    /// Options → AI's Test.
    Tested { job: u64, result: Result<(), AiError> },
    /// A Grok sign-in's code, to show until it ends; `page` is xAI's page
    /// with the code filled in.
    DeviceCode { job: u64, code: String, page: String },
    /// A sign-in (ChatGPT or Grok) ended: the email signed in, or why not.
    SignedIn { job: u64, provider: Provider, result: Result<String, AiError> },
}

/// Why a request gave no answer, as users are told.
#[derive(Debug, Clone, PartialEq)]
pub enum AiError {
    NoProvider,
    NoKey,
    /// The custom provider has no `base_url`.
    NoAddress,
    NoModel,
    /// 401 or 403.
    BadKey,
    /// 429.
    RateLimited,
    /// The model declined (`stop_reason: refusal`).
    Refused,
    /// The reply hit its length limit before it was useful.
    TooLong,
    /// No answer from the server: no network, DNS, TLS, timeout.
    Network(String),
    /// Another HTTP error, with the server's message.
    Api { status: u16, message: String },
    /// An answer that can't be read.
    BadReply(String),
    /// The ChatGPT plan can't be used by other apps (Plus or Pro is needed).
    PlanNotEligible,
    /// The ChatGPT plan's limit for this app is reached.
    PlanLimit,
    /// ChatGPT can't take plan requests just now.
    PlanBusy,
    /// The sign-in (ChatGPT or Grok) is no longer valid.
    SignInAgain,
    /// xAI doesn't let the Grok plan be used by other apps.
    GrokPlan,
    /// No Claude Code was found.
    NoClaudeCode,
    /// Claude Code is not signed in.
    ClaudeCodeSignIn,
    /// Signing in went wrong; says how.
    Login(String),
    /// The model kept calling tools past `fix::MAX_REQUESTS` requests.
    TooManySteps,
    /// The user cancelled (never shown).
    Cancelled,
}

impl std::fmt::Display for AiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AiError::NoProvider => write!(f, "no AI provider is set (Options → AI)"),
            AiError::NoKey => write!(f, "no API key is set (Options → AI)"),
            AiError::NoAddress => write!(f, "the custom provider has no address: set base_url in config.toml"),
            AiError::NoModel => write!(f, "no model is chosen (Options → AI)"),
            AiError::BadKey => write!(f, "the API key was not accepted"),
            AiError::RateLimited => write!(f, "too many requests or no credit left; try again later"),
            AiError::Refused => write!(f, "the model declined to answer"),
            AiError::TooLong => write!(f, "the reply was cut off"),
            AiError::Network(e) => write!(f, "could not reach the provider: {e}"),
            AiError::Api { status, message } => write!(f, "error {status}: {message}"),
            AiError::BadReply(e) => write!(f, "unexpected reply: {e}"),
            AiError::PlanNotEligible => write!(f, "your ChatGPT plan can't be used here (Plus or Pro is needed)"),
            AiError::PlanLimit => write!(f, "you have reached your ChatGPT plan's limit; see Manage usage (Options → AI)"),
            AiError::PlanBusy => write!(f, "ChatGPT is busy; try again"),
            AiError::SignInAgain => write!(f, "sign in again (Options → AI)"),
            AiError::NoClaudeCode => write!(f, "Claude Code was not found: install it (claude.com/code){}", if cfg!(windows) { " in Windows or WSL" } else { "" }),
            AiError::ClaudeCodeSignIn => write!(f, "Claude Code is not signed in: run claude in a terminal and sign in"),
            AiError::GrokPlan => write!(f, "xAI didn't allow your Grok plan here (SuperGrok or X Premium is needed); you can use an API key instead"),
            AiError::Login(e) => write!(f, "{e}"),
            AiError::TooManySteps => write!(f, "the AI took too many steps"),
            AiError::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// Who answers: a provider and the API format it speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    /// No provider: the AI items open Options → AI.
    #[default]
    #[serde(rename = "")]
    Off,
    /// OpenAI (ChatGPT), Chat Completions format.
    #[serde(rename = "openai")]
    OpenAi,
    /// xAI (Grok), Chat Completions format.
    Xai,
    /// Anthropic (Claude), Messages API.
    Anthropic,
    /// The user's installed Claude Code (`claude -p`), with its own sign-in.
    #[serde(rename = "claude-code")]
    ClaudeCode,
    /// OpenRouter: many providers' models with one key, some free.
    #[serde(rename = "openrouter")]
    OpenRouter,
    /// Any server with the Chat Completions format at `[ai] base_url`,
    /// such as a local Ollama.
    Custom,
}

impl Provider {
    /// The providers Options → AI offers, after "Off".
    pub const CHOICES: [Provider; 6] = [Provider::OpenAi, Provider::Xai, Provider::Anthropic, Provider::ClaudeCode, Provider::OpenRouter, Provider::Custom];

    pub fn label(self) -> &'static str {
        match self {
            Provider::Off => "Off",
            Provider::OpenAi => "ChatGPT",
            Provider::Xai => "Grok",
            Provider::Anthropic => "Claude",
            Provider::ClaudeCode => "Claude Code",
            Provider::OpenRouter => "OpenRouter",
            Provider::Custom => "Custom",
        }
    }

    /// The name in `ai-keys.toml`, as in `config.toml`.
    pub fn name(self) -> &'static str {
        match self {
            Provider::Off => "",
            Provider::OpenAi => "openai",
            Provider::Xai => "xai",
            Provider::Anthropic => "anthropic",
            Provider::ClaudeCode => "claude-code",
            Provider::OpenRouter => "openrouter",
            Provider::Custom => "custom",
        }
    }

    /// The environment variable read when `ai-keys.toml` has no key.
    pub fn env_var(self) -> Option<&'static str> {
        match self {
            Provider::OpenAi => Some("OPENAI_API_KEY"),
            Provider::Xai => Some("XAI_API_KEY"),
            Provider::Anthropic => Some("ANTHROPIC_API_KEY"),
            Provider::OpenRouter => Some("OPENROUTER_API_KEY"),
            Provider::Off | Provider::ClaudeCode | Provider::Custom => None,
        }
    }

    /// The model used when none is chosen: Claude's newest, and on
    /// OpenRouter its router to free models. The others list many, and a
    /// built-in name would go stale.
    pub fn default_model(self) -> &'static str {
        match self {
            Provider::Anthropic => "claude-opus-5-5",
            Provider::ClaudeCode => "sonnet",
            Provider::OpenRouter => openrouter::FREE_ROUTER,
            _ => "",
        }
    }

    /// Speaks Anthropic's Messages API rather than Chat Completions.
    pub fn uses_messages_api(self) -> bool {
        self == Provider::Anthropic
    }
}

/// The `[ai]` table of `config.toml`. The keys are not here (see [`keys`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiConfig {
    /// "openai", "xai", "anthropic", "claude-code", "openrouter", "custom",
    /// or "" for no AI.
    pub provider: Provider,
    /// The model, as the provider names it; "" for the provider's default.
    pub model: String,
    /// For "custom": the address that has `/chat/completions`, such as
    /// `http://localhost:11434/v1`.
    pub base_url: String,
    /// How many recent commands a Fix lists (their lines; output is sent
    /// for the last one and on request).
    pub listed_commands: usize,
    /// The chat panel's share of the window's width.
    pub panel_width: f32,
    /// Providers the user agreed to send terminal text to.
    pub confirmed: Vec<Provider>,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            provider: Provider::Off,
            model: String::new(),
            base_url: String::new(),
            listed_commands: 50,
            panel_width: 0.4,
            confirmed: Vec::new(),
        }
    }
}

impl AiConfig {
    /// The model to ask: the one chosen, else the provider's default.
    pub fn model_or_default(&self) -> String {
        if self.model.is_empty() { self.provider.default_model().to_owned() } else { self.model.clone() }
    }

    pub fn sanitized(mut self) -> Self {
        self.listed_commands = self.listed_commands.clamp(1, 200);
        self.panel_width = self.panel_width.clamp(0.2, 0.8);
        self
    }
}
