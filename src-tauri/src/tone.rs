//! Tone that fits the app: a dictation into Slack reads like a chat message,
//! one into Mail like an email, and one into a terminal like a command.
//!
//! The app with focus when dictation starts is sorted into a category, from
//! the user's own list first and then the built-in one below. With AI cleanup
//! on, the category adds a line to the model's instructions. Without it, a few
//! light rules do what they safely can, like dropping the final period in chat.
//! In code editors and terminals, spoken file names and identifiers are also
//! written as code (see `coding`).

use std::{
    sync::{Mutex, mpsc},
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::{LockExt, cleanup::Hint, coding, platform::FrontApp};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AppCategory {
    Chat,
    Email,
    Terminal,
    Code,
    /// Documents, notes and everything else: no change in tone.
    Other,
}

/// The user's choice of category for one app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppRule {
    /// An app's name, bundle id (macOS), executable (Windows) or window class
    /// (Linux), as shown under "Last app" in Settings. Case doesn't matter.
    pub app: String,
    pub category: AppCategory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ToneSettings {
    /// Match the tone to the app: instructions for AI cleanup, light rules without it.
    pub enabled: bool,
    /// Write spoken file names, identifiers and flags as code in code editors and terminals.
    pub format_code: bool,
    /// The user's categories, checked before the built-in ones.
    pub apps: Vec<AppRule>,
}

impl Default for ToneSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            format_code: true,
            apps: Vec::new(),
        }
    }
}

/// Known apps: the names they go by (lowercase bundle ids, executables,
/// window classes and app names), a name for Settings, and their category.
/// A name ending in `*` matches anything starting with the rest.
#[rustfmt::skip]
const BUILT_IN: &[(&[&str], &str, AppCategory)] = {
    use AppCategory::*;
    &[
        // Chat
        (&["com.tinyspeck.slackmacgap", "slack"], "Slack", Chat),
        (&["com.hnc.discord", "discord"], "Discord", Chat),
        (&["com.apple.mobilesms", "messages"], "Messages", Chat),
        (&["net.whatsapp.whatsapp", "whatsapp", "whatsapp.root"], "WhatsApp", Chat),
        (&["ru.keepcoder.telegram", "org.telegram.desktop", "telegram", "telegramdesktop",
           "telegram desktop"], "Telegram", Chat),
        (&["com.microsoft.teams", "com.microsoft.teams2", "teams", "ms-teams", "msteams",
           "microsoft teams"], "Microsoft Teams", Chat),
        (&["org.whispersystems.signal-desktop", "signal"], "Signal", Chat),
        (&["com.facebook.archon", "messenger"], "Messenger", Chat),
        (&["im.riot.app", "element"], "Element", Chat),
        (&["com.mattermost.desktop", "mattermost"], "Mattermost", Chat),
        (&["org.zulip.zulip-electron", "zulip"], "Zulip", Chat),
        (&["com.tencent.xinwechat", "wechat"], "WeChat", Chat),
        (&["com.skype.skype", "skype"], "Skype", Chat),
        // Email
        (&["com.apple.mail", "mail"], "Mail", Email),
        (&["com.microsoft.outlook", "outlook", "olk"], "Outlook", Email),
        (&["org.mozilla.thunderbird", "thunderbird", "mozilla thunderbird"], "Thunderbird", Email),
        (&["com.readdle.smartemail-mac", "spark", "spark desktop"], "Spark", Email),
        (&["com.superhuman.electron", "superhuman"], "Superhuman", Email),
        (&["it.bloop.airmail2", "airmail"], "Airmail", Email),
        (&["com.mailspring.mailspring", "mailspring"], "Mailspring", Email),
        (&["org.gnome.evolution", "evolution"], "Evolution", Email),
        (&["org.gnome.geary", "geary"], "Geary", Email),
        (&["org.kde.kmail2", "kmail"], "KMail", Email),
        // Terminals
        (&["com.apple.terminal", "terminal"], "Terminal", Terminal),
        (&["com.googlecode.iterm2", "iterm2", "iterm"], "iTerm2", Terminal),
        (&["windowsterminal", "windows terminal", "openconsole"], "Windows Terminal", Terminal),
        (&["cmd", "conhost", "powershell", "pwsh"], "Command Prompt and PowerShell", Terminal),
        (&["dev.warp.warp-stable", "warp"], "Warp", Terminal),
        (&["io.alacritty", "org.alacritty", "alacritty"], "Alacritty", Terminal),
        (&["net.kovidgoyal.kitty", "kitty"], "kitty", Terminal),
        (&["com.github.wez.wezterm", "org.wezfurlong.wezterm", "wezterm", "wezterm-gui"],
         "WezTerm", Terminal),
        (&["com.mitchellh.ghostty", "ghostty"], "Ghostty", Terminal),
        (&["co.zeit.hyper", "hyper"], "Hyper", Terminal),
        (&["org.tabby", "tabby"], "Tabby", Terminal),
        (&["gnome-terminal", "gnome-terminal-server", "org.gnome.terminal", "org.gnome.console",
           "kgx"], "GNOME Terminal", Terminal),
        (&["org.gnome.ptyxis", "ptyxis"], "Ptyxis", Terminal),
        (&["org.kde.konsole", "konsole", "yakuake"], "Konsole", Terminal),
        (&["xterm", "urxvt", "st", "st-256color", "foot", "tilix", "terminator", "xfce4-terminal",
           "mate-terminal", "lxterminal", "qterminal", "guake", "terminology", "blackbox",
           "cool-retro-term"], "Other Linux terminals", Terminal),
        // Code editors
        (&["com.microsoft.vscode", "com.microsoft.vscodeinsiders", "code", "code - insiders",
           "code-insiders", "visual studio code"], "VS Code", Code),
        (&["com.vscodium", "vscodium", "codium"], "VSCodium", Code),
        (&["com.todesktop.230313mzl4w4u92", "cursor"], "Cursor", Code),
        (&["com.exafunction.windsurf", "windsurf"], "Windsurf", Code),
        (&["com.t3tools.*", "t3 code*"], "T3 Code", Code),
        (&["dev.zed.zed", "dev.zed.zed-preview", "zed", "zed-editor"], "Zed", Code),
        (&["com.apple.dt.xcode", "xcode"], "Xcode", Code),
        (&["com.jetbrains.*", "jetbrains-*", "idea", "idea64", "intellij idea", "pycharm",
           "pycharm64", "webstorm", "webstorm64", "clion", "clion64", "goland", "goland64",
           "rider", "rider64", "rubymine", "rubymine64", "phpstorm", "phpstorm64", "datagrip",
           "datagrip64", "rustrover", "rustrover64", "fleet"], "JetBrains IDEs", Code),
        (&["com.google.android.studio", "studio64", "android studio"], "Android Studio", Code),
        (&["com.sublimetext.4", "com.sublimetext.3", "sublime_text", "sublime text"],
         "Sublime Text", Code),
        (&["com.panic.nova", "nova"], "Nova", Code),
        (&["devenv", "visual studio"], "Visual Studio", Code),
        (&["notepad++"], "Notepad++", Code),
        (&["org.gnu.emacs", "emacs", "neovide", "gvim"], "Neovim, Vim and Emacs", Code),
    ]
};

/// Browsers, where the window title tells which web app is open.
const BROWSERS: &[&str] = &[
    "com.google.chrome",
    "com.google.chrome.canary",
    "google-chrome",
    "chrome",
    "google chrome",
    "chromium",
    "chromium-browser",
    "org.chromium.chromium",
    "com.apple.safari",
    "safari",
    "org.mozilla.firefox",
    "firefox",
    "firefox-esr",
    "navigator",
    "com.microsoft.edgemac",
    "msedge",
    "microsoft-edge",
    "microsoft edge",
    "com.brave.browser",
    "brave",
    "brave-browser",
    "company.thebrowser.browser",
    "arc",
    "com.vivaldi.vivaldi",
    "vivaldi",
    "vivaldi-stable",
    "com.operasoftware.opera",
    "opera",
    "app.zen-browser.zen",
    "zen",
];

/// Words in a browser window's title that name a web app.
const WEB_APPS: &[(&str, AppCategory)] = &[
    ("gmail", AppCategory::Email),
    ("outlook", AppCategory::Email),
    ("proton mail", AppCategory::Email),
    ("fastmail", AppCategory::Email),
    ("yahoo mail", AppCategory::Email),
    ("icloud mail", AppCategory::Email),
    ("slack", AppCategory::Chat),
    ("discord", AppCategory::Chat),
    ("whatsapp", AppCategory::Chat),
    ("messenger", AppCategory::Chat),
    ("telegram", AppCategory::Chat),
    ("microsoft teams", AppCategory::Chat),
    ("google chat", AppCategory::Chat),
];

/// "Slack.exe", " slack " and "SLACK" all become "slack".
fn normalize(name: &str) -> String {
    let name = name.trim().to_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    name.trim().to_string()
}

fn matches(pattern: &str, name: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => pattern == name,
    }
}

/// Whether `phrase` appears in `text` as whole words.
fn contains_words(text: &str, phrase: &str) -> bool {
    text.match_indices(phrase).any(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = text[start + phrase.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

/// Sorts `app` into a category: the user's own `rules` first, then the
/// built-in list, then web apps in a browser by the window title.
pub fn classify(app: &FrontApp, rules: &[AppRule]) -> AppCategory {
    let names: Vec<String> = [&app.id, &app.name]
        .into_iter()
        .map(|name| normalize(name))
        .filter(|name| !name.is_empty())
        .collect();
    let known = |pattern: &str| names.iter().any(|name| matches(pattern, name));

    if let Some(rule) = rules
        .iter()
        .find(|rule| !normalize(&rule.app).is_empty() && known(&normalize(&rule.app)))
    {
        return rule.category;
    }
    if let Some((_, _, category)) = BUILT_IN
        .iter()
        .find(|(patterns, _, _)| patterns.iter().any(|p| known(p)))
    {
        return *category;
    }
    if BROWSERS.iter().any(|browser| known(browser)) {
        let title = app.title.to_lowercase();
        if let Some((_, category)) = WEB_APPS
            .iter()
            .find(|(word, _)| contains_words(&title, word))
        {
            return *category;
        }
    }
    AppCategory::Other
}

/// Whether `app` is a browser showing Google Search. Its search box drops
/// pasted text once the clipboard has changed since the last paste into it,
/// as it does whenever the clipboard is restored after a dictation.
pub fn is_google_search(app: &FrontApp) -> bool {
    let names = [&app.id, &app.name].map(|name| normalize(name));
    if !BROWSERS
        .iter()
        .any(|browser| names.iter().any(|name| matches(browser, name)))
    {
        return false;
    }
    // "Google - Brave" on the home page, "cats - Google Search - Brave" on results.
    app.title
        .to_lowercase()
        .split(" - ")
        .any(|part| matches!(part.trim(), "google" | "google search"))
}

/// The built-in apps by category, for Settings.
pub fn built_in_apps() -> Vec<(&'static str, AppCategory)> {
    BUILT_IN
        .iter()
        .map(|(_, name, category)| (*name, *category))
        .collect()
}

/// The last app dictated into, so Settings can show what an app is called.
static LAST_APP: Mutex<Option<FrontApp>> = Mutex::new(None);

/// The app with focus now, remembered for Settings.
fn detect() -> Option<FrontApp> {
    let app = crate::platform::frontmost_app()?;
    *LAST_APP.lock_unpoisoned() = Some(app.clone());
    Some(app)
}

/// How long a finished recording waits for the app with focus before going
/// on without it, in the default style.
const DETECTION_WAIT: Duration = Duration::from_secs(1);

/// Finding the app with focus, on a thread of its own: it can take a moment
/// (on macOS the window title comes from Accessibility, which may wait for an
/// app that's slow to answer), and the hotkey mustn't wait for it.
pub struct Detection(Option<mpsc::Receiver<Option<FrontApp>>>);

impl Detection {
    /// Starts looking for the app with focus now.
    pub fn start() -> Self {
        Self::spawn(detect)
    }

    fn spawn(find: impl FnOnce() -> Option<FrontApp> + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        let started = thread::Builder::new()
            .name("app-detection".into())
            .spawn(move || {
                let _ = tx.send(find());
            })
            .map_err(|e| eprintln!("[tone] couldn't start app detection: {e}"))
            .is_ok();
        Self(started.then_some(rx))
    }

    /// The app that had focus. An app that never answers mustn't hold up the
    /// transcription, so this gives up after `DETECTION_WAIT`.
    pub fn app(self) -> Option<FrontApp> {
        let app = self.0?.recv_timeout(DETECTION_WAIT);
        if app.is_err() {
            eprintln!("[tone] the app with focus didn't answer in time");
        }
        app.ok().flatten()
    }
}

pub fn last_app() -> Option<FrontApp> {
    LAST_APP.lock_unpoisoned().clone()
}

/// What a dictation into one app gets: its category, and which of the
/// tone and code features are on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub category: AppCategory,
    tone: bool,
    format_code: bool,
}

impl Style {
    /// No app, or both features off: text passes through unchanged.
    pub fn new(settings: &ToneSettings, app: Option<&FrontApp>) -> Self {
        Self {
            category: app.map_or(AppCategory::Other, |app| classify(app, &settings.apps)),
            tone: settings.enabled,
            format_code: settings.format_code,
        }
    }

    fn is_code(&self) -> bool {
        matches!(self.category, AppCategory::Code | AppCategory::Terminal)
    }

    /// Writes spoken identifiers as code, in code editors and terminals.
    /// Runs before AI cleanup, so the model sees them already written.
    pub fn format(&self, text: &str) -> String {
        if self.format_code && self.is_code() {
            coding::format(text, self.category == AppCategory::Terminal)
        } else {
            text.to_string()
        }
    }

    /// How AI cleanup should write for this app: an instruction, examples in
    /// that style, and whether spoken lists become one item per line.
    pub fn hint(&self) -> Option<Hint> {
        let code = (self.format_code && self.is_code()).then_some(
            "Write file names, paths, identifiers and command-line flags exactly as code, \
             like main.rs, user_id, getUserName or --verbose.",
        );
        let (instruction, examples, lists): (_, &'static [(&str, &str)], _) =
            match (self.tone, self.category) {
                (true, AppCategory::Chat) => (
                    "The text is a chat message. Keep it casual and short, and leave out \
                     the period at the end.",
                    CHAT_EXAMPLES,
                    false,
                ),
                (true, AppCategory::Email) => (
                    "The text is for an email. Write complete, properly punctuated sentences, \
                     and split separate points into paragraphs. Put a dictated greeting or \
                     sign-off on its own line.",
                    EMAIL_EXAMPLES,
                    true,
                ),
                (true, AppCategory::Terminal) => (
                    "The text is typed into a terminal. When it's a command, write only the \
                     command exactly as it would be typed, with no capital letter, quotes or \
                     period added around it.",
                    TERMINAL_EXAMPLES,
                    false,
                ),
                (true, AppCategory::Code) => (
                    "The text is for a code editor, such as a comment, a commit message or \
                     a prompt to an AI assistant. Write it plainly.",
                    &[],
                    true,
                ),
                _ => ("", &[], true),
            };
        let instruction = [instruction, code.unwrap_or_default()]
            .into_iter()
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        (!instruction.is_empty()).then_some(Hint {
            instruction,
            examples,
            lists,
        })
    }

    /// Rules that hold after AI cleanup too, for what a small model misses:
    /// a chat message never ends in a period. Terminal rules don't apply here,
    /// as the model can tell a command from a sentence and they can't.
    pub fn adjust_cleaned(&self, text: &str) -> String {
        if self.tone && self.category == AppCategory::Chat {
            drop_final_period(text)
        } else {
            text.to_string()
        }
    }

    /// Light rules for when AI cleanup doesn't run.
    pub fn adjust(&self, text: &str) -> String {
        if !self.tone {
            return text.to_string();
        }
        match self.category {
            AppCategory::Chat => drop_final_period(text),
            AppCategory::Terminal => as_command(text),
            _ => text.to_string(),
        }
    }
}

/// AI cleanup examples for chat: casual, no final period, lists kept in
/// the sentence. Fillers and self-corrections still go.
const CHAT_EXAMPLES: &[(&str, &str)] = &[
    (
        "um yeah that works for me uh see you at 3",
        "Yeah that works for me, see you at 3",
    ),
    (
        "did you get a chance to look at the the doc",
        "Did you get a chance to look at the doc?",
    ),
    (
        "lets get pizza tonight no wait sushi",
        "Let's get sushi tonight",
    ),
    (
        "can you grab eggs milk and bread on the way home",
        "Can you grab eggs, milk and bread on the way home?",
    ),
];

/// AI cleanup examples for email: greeting and sign-off on their own lines.
const EMAIL_EXAMPLES: &[(&str, &str)] = &[
    (
        "hi sam um thanks for sending the report i'll go through it tomorrow and get back to you best alex",
        "Hi Sam,\n\nThanks for sending the report. I'll go through it tomorrow and get back to you.\n\nBest,\nAlex",
    ),
    (
        "can we move the call to tuesday no wait wednesday at 3",
        "Can we move the call to Wednesday at 3?",
    ),
    (
        "before the meeting please send the budget the timeline and the slides",
        "Before the meeting, please send:\n- The budget\n- The timeline\n- The slides",
    ),
];

/// AI cleanup examples for terminals: commands as typed, sentences (to a
/// coding agent, say) as sentences.
const TERMINAL_EXAMPLES: &[(&str, &str)] = &[
    ("git status", "git status"),
    ("um npm run dev", "npm run dev"),
    (
        "so the tests uh the tests fail when i run them twice",
        "The tests fail when I run them twice.",
    ),
];

/// "See you then." → "See you then". Ellipses, abbreviations ("3 p.m.",
/// "etc.") and text with no period stay.
fn drop_final_period(text: &str) -> String {
    let trimmed = text.trim_end();
    match trimmed.strip_suffix('.') {
        Some(rest) if !rest.ends_with('.') && !rest.is_empty() && !is_abbreviation(rest) => {
            rest.to_string()
        }
        _ => text.to_string(),
    }
}

/// Whether the period after `text`'s last word belongs to it, as in "p.m."
/// or "etc.".
fn is_abbreviation(text: &str) -> bool {
    let word = text.rsplit(char::is_whitespace).next().unwrap_or_default();
    word.contains('.')
        || ["etc", "vs", "approx", "inc", "ltd", "co", "jr", "sr"]
            .contains(&word.to_lowercase().as_str())
}

/// "Git status." → "git status": no capital letter or trailing punctuation.
/// Words in capitals, like "README" or "I", keep theirs.
fn as_command(text: &str) -> String {
    let trimmed = text
        .trim()
        .trim_end_matches(['.', ',', ';', '!'])
        .trim_end();
    let first_word = trimmed.split_whitespace().next().unwrap_or_default();
    let mut chars = first_word.chars();
    let lower_rest = chars.next().is_some_and(char::is_uppercase)
        && chars.next().is_some_and(char::is_lowercase)
        && !first_word.starts_with("I'");
    if !lower_rest {
        return trimmed.to_string();
    }
    let mut chars = trimmed.chars();
    chars
        .next()
        .map(|first| first.to_lowercase().chain(chars).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_app_that_never_answers_doesnt_hold_up_the_dictation() {
        let started = std::time::Instant::now();
        let slow = Detection::spawn(|| {
            thread::sleep(DETECTION_WAIT * 5);
            None
        });
        assert_eq!(slow.app(), None);
        assert!(started.elapsed() < DETECTION_WAIT * 3);

        let quick = Detection::spawn(|| Some(app("com.tinyspeck.slackmacgap", "Slack", "")));
        assert_eq!(quick.app().map(|a| a.name), Some("Slack".into()));
    }

    fn app(id: &str, name: &str, title: &str) -> FrontApp {
        FrontApp {
            id: id.into(),
            name: name.into(),
            title: title.into(),
        }
    }

    fn category(id: &str) -> AppCategory {
        classify(&app(id, "", ""), &[])
    }

    #[test]
    fn known_apps_on_each_system() {
        // macOS bundle ids, Windows executables, Linux window classes.
        assert_eq!(category("com.tinyspeck.slackmacgap"), AppCategory::Chat);
        assert_eq!(category("Slack.exe"), AppCategory::Chat);
        assert_eq!(category("discord"), AppCategory::Chat);
        assert_eq!(category("com.apple.MobileSMS"), AppCategory::Chat);
        assert_eq!(category("com.apple.mail"), AppCategory::Email);
        assert_eq!(category("OUTLOOK"), AppCategory::Email);
        assert_eq!(category("olk"), AppCategory::Email);
        assert_eq!(category("thunderbird"), AppCategory::Email);
        assert_eq!(category("com.googlecode.iterm2"), AppCategory::Terminal);
        assert_eq!(category("com.t3tools.t3code"), AppCategory::Code);
        assert_eq!(category("T3 Code (Beta)"), AppCategory::Code);
        assert_eq!(category("WindowsTerminal"), AppCategory::Terminal);
        assert_eq!(category("gnome-terminal-server"), AppCategory::Terminal);
        assert_eq!(category("kitty"), AppCategory::Terminal);
        assert_eq!(category("org.wezfurlong.wezterm"), AppCategory::Terminal);
        assert_eq!(category("com.microsoft.VSCode"), AppCategory::Code);
        assert_eq!(category("Code.exe"), AppCategory::Code);
        assert_eq!(category("cursor"), AppCategory::Code);
        assert_eq!(category("com.jetbrains.intellij"), AppCategory::Code);
        assert_eq!(category("jetbrains-pycharm"), AppCategory::Code);
        assert_eq!(category("dev.zed.Zed"), AppCategory::Code);
        assert_eq!(category("com.apple.dt.Xcode"), AppCategory::Code);
        assert_eq!(category("com.apple.TextEdit"), AppCategory::Other);
        assert_eq!(category(""), AppCategory::Other);
    }

    #[test]
    fn the_app_name_counts_too() {
        assert_eq!(classify(&app("", "Slack", ""), &[]), AppCategory::Chat);
        assert_eq!(
            classify(&app("Gnome-terminal", "Gnome-terminal", ""), &[]),
            AppCategory::Terminal
        );
    }

    #[test]
    fn web_apps_in_a_browser_go_by_the_window_title() {
        let gmail = app(
            "com.google.Chrome",
            "Google Chrome",
            "Inbox (3) - me@example.com - Gmail",
        );
        assert_eq!(classify(&gmail, &[]), AppCategory::Email);
        let slack = app(
            "firefox",
            "Firefox",
            "general - Acme - Slack — Mozilla Firefox",
        );
        assert_eq!(classify(&slack, &[]), AppCategory::Chat);
        let docs = app("msedge", "msedge", "Quarterly plan - Google Docs");
        assert_eq!(classify(&docs, &[]), AppCategory::Other);
        // Only whole words, and only in browsers.
        let other = app("com.google.Chrome", "", "Slacker news");
        assert_eq!(classify(&other, &[]), AppCategory::Other);
        let editor = app("com.apple.TextEdit", "TextEdit", "Gmail notes");
        assert_eq!(classify(&editor, &[]), AppCategory::Other);
    }

    #[test]
    fn google_search_is_found_by_the_window_title() {
        let home = app("com.brave.Browser", "Brave Browser", "Google - Brave");
        assert!(is_google_search(&home));
        let results = app(
            "com.google.Chrome",
            "Google Chrome",
            "cats - Google Search - Google Chrome",
        );
        assert!(is_google_search(&results));
        let docs = app("com.google.Chrome", "", "Quarterly plan - Google Docs");
        assert!(!is_google_search(&docs));
        let editor = app("com.apple.TextEdit", "TextEdit", "Google");
        assert!(!is_google_search(&editor));
    }

    #[test]
    fn the_users_choice_comes_first() {
        let rules = [
            AppRule {
                app: " Slack ".into(),
                category: AppCategory::Other,
            },
            AppRule {
                app: "obsidian".into(),
                category: AppCategory::Code,
            },
            AppRule {
                app: "".into(),
                category: AppCategory::Chat,
            },
        ];
        assert_eq!(
            classify(&app("com.tinyspeck.slackmacgap", "Slack", ""), &rules),
            AppCategory::Other
        );
        assert_eq!(
            classify(&app("md.obsidian", "Obsidian", ""), &rules),
            AppCategory::Code
        );
        assert_eq!(
            classify(&app("com.apple.TextEdit", "TextEdit", ""), &rules),
            AppCategory::Other,
            "an empty rule matches nothing"
        );
    }

    #[test]
    fn every_built_in_app_has_a_name() {
        for (patterns, name, _) in BUILT_IN {
            assert!(!name.is_empty() && !patterns.is_empty());
            for pattern in *patterns {
                assert_eq!(
                    *pattern,
                    normalize(pattern),
                    "{pattern} should be lowercase"
                );
            }
        }
        assert_eq!(built_in_apps().len(), BUILT_IN.len());
    }

    fn style(category: AppCategory) -> Style {
        Style {
            category,
            tone: true,
            format_code: true,
        }
    }

    #[test]
    fn chat_drops_the_final_period() {
        let chat = style(AppCategory::Chat);
        assert_eq!(
            chat.adjust("Sounds good, see you then."),
            "Sounds good, see you then"
        );
        assert_eq!(chat.adjust("Wait for it..."), "Wait for it...");
        assert_eq!(chat.adjust("Are you there?"), "Are you there?");
        assert_eq!(chat.adjust("First. Second."), "First. Second");
        assert_eq!(chat.adjust("."), ".");
        assert_eq!(chat.adjust("See you at 3 p.m."), "See you at 3 p.m.");
        assert_eq!(chat.adjust("Snacks, drinks, etc."), "Snacks, drinks, etc.");
    }

    #[test]
    fn terminals_get_commands() {
        let terminal = style(AppCategory::Terminal);
        assert_eq!(terminal.adjust("Git status."), "git status");
        assert_eq!(terminal.adjust("Npm run dev!"), "npm run dev");
        assert_eq!(terminal.adjust("README.md"), "README.md");
        assert_eq!(terminal.adjust("I think so."), "I think so");
        assert_eq!(terminal.adjust("What is this?"), "what is this?");
        assert_eq!(terminal.adjust("ls -la"), "ls -la");
    }

    #[test]
    fn other_apps_and_tone_off_are_left_alone() {
        assert_eq!(style(AppCategory::Email).adjust("Hi Sam."), "Hi Sam.");
        assert_eq!(style(AppCategory::Other).adjust("Done."), "Done.");
        let off = Style {
            tone: false,
            ..style(AppCategory::Chat)
        };
        assert_eq!(off.adjust("See you."), "See you.");
        assert_eq!(off.hint(), None);
    }

    #[test]
    fn chat_drops_the_final_period_after_cleanup_too() {
        let chat = style(AppCategory::Chat);
        assert_eq!(chat.adjust_cleaned("See you at 3."), "See you at 3");
        let terminal = style(AppCategory::Terminal);
        assert_eq!(
            terminal.adjust_cleaned("Fix the login bug."),
            "Fix the login bug."
        );
        let off = Style {
            tone: false,
            ..chat
        };
        assert_eq!(off.adjust_cleaned("See you."), "See you.");
    }

    #[test]
    fn style_examples_follow_their_own_rules() {
        let chat = style(AppCategory::Chat);
        for (_, cleaned) in CHAT_EXAMPLES {
            assert_eq!(chat.adjust_cleaned(cleaned), *cleaned);
            assert!(!cleaned.contains('\n'));
        }
        for (_, cleaned) in TERMINAL_EXAMPLES {
            assert!(!cleaned.contains('\n'));
        }
    }

    #[test]
    fn hints_describe_the_app() {
        let chat = style(AppCategory::Chat).hint().unwrap();
        assert!(chat.instruction.contains("chat message"));
        assert_eq!(chat.examples, CHAT_EXAMPLES);
        assert!(!chat.lists);
        let email = style(AppCategory::Email).hint().unwrap();
        assert!(email.instruction.contains("email"));
        assert!(email.lists);
        let terminal = style(AppCategory::Terminal).hint().unwrap();
        assert!(
            terminal.instruction.contains("command") && terminal.instruction.contains("--verbose")
        );
        assert!(!terminal.lists);
        let code_only = Style {
            tone: false,
            ..style(AppCategory::Code)
        };
        assert!(
            code_only
                .hint()
                .unwrap()
                .instruction
                .starts_with("Write file names")
        );
        assert_eq!(style(AppCategory::Other).hint(), None);
    }

    #[test]
    fn code_formatting_only_in_code_apps() {
        assert_eq!(
            style(AppCategory::Code).format("open main dot rs"),
            "open main.rs"
        );
        assert_eq!(
            style(AppCategory::Chat).format("main dot rs"),
            "main dot rs"
        );
        let off = Style {
            format_code: false,
            ..style(AppCategory::Code)
        };
        assert_eq!(off.format("main dot rs"), "main dot rs");
    }

    #[test]
    fn no_app_means_no_change() {
        let style = Style::new(&ToneSettings::default(), None);
        assert_eq!(style.category, AppCategory::Other);
        assert_eq!(style.adjust("Hello."), "Hello.");
        assert_eq!(style.hint(), None);
    }
}
