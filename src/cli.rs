use std::ffi::OsString;

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Help,
    Version,
    All,
    Cards,
    Last4(String),
}

#[derive(Debug, PartialEq, Eq)]
pub struct Parsed {
    pub action: Action,
    pub config: Option<String>,
    /// Poll mode: fetch at most this many pages per category (at least 1).
    pub pages: Option<usize>,
    /// Poll mode: keep only events created at or after this epoch-millisecond time.
    pub since: Option<i64>,
}

/// Upper bound for `--pages`; equals the full-history safety bound.
pub const MAX_PAGES_OPTION: usize = 10000;

pub fn parse<I>(args: I) -> Result<Parsed, String>
where
    I: IntoIterator<Item = OsString>,
{
    let values: Vec<String> = args
        .into_iter()
        .map(|value| {
            value
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| "invalid non-UTF-8 argument".to_string())
        })
        .collect::<Result<_, _>>()?;
    let mut action: Option<Action> = None;
    let mut config = None;
    let mut pages: Option<usize> = None;
    let mut since: Option<i64> = None;
    let mut i = 0;
    while i < values.len() {
        match values[i].as_str() {
            "--help" | "-h" => action = Some(Action::Help),
            "--version" => action = Some(Action::Version),
            "--all" => set_action(&mut action, Action::All)?,
            "--cards" => set_action(&mut action, Action::Cards)?,
            "--config" => {
                i += 1;
                if i >= values.len() || values[i].is_empty() || values[i].starts_with('-') {
                    return Err("--config requires a path".into());
                }
                config = Some(values[i].clone());
            }
            "--since" => {
                i += 1;
                let text = values.get(i).map(String::as_str).unwrap_or("");
                let stamp = (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
                    .then(|| text.parse::<i64>().ok())
                    .flatten()
                    .ok_or_else(|| "--since requires a Unix time in milliseconds".to_string())?;
                if since.replace(stamp).is_some() {
                    return Err("--since may be given only once".into());
                }
            }
            "--pages" => {
                i += 1;
                let text = values.get(i).map(String::as_str).unwrap_or("");
                let count = (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
                    .then(|| text.parse::<usize>().ok())
                    .flatten()
                    .filter(|count| (1..=MAX_PAGES_OPTION).contains(count))
                    .ok_or_else(|| {
                        format!("--pages requires a whole number from 1 to {MAX_PAGES_OPTION}")
                    })?;
                if pages.replace(count).is_some() {
                    return Err("--pages may be given only once".into());
                }
            }
            value if value.starts_with('-') => return Err("invalid arguments; use --help".into()),
            value => {
                let candidate = value.to_owned();
                if !candidate
                    .as_bytes()
                    .iter()
                    .all(|byte| byte.is_ascii_digit())
                    || candidate.len() != 4
                {
                    return Err("the card ending must contain exactly four ASCII digits".into());
                }
                set_action(&mut action, Action::Last4(candidate))?;
            }
        }
        i += 1;
    }
    if (pages.is_some() || since.is_some()) && matches!(action, None | Some(Action::Cards)) {
        return Err(
            "--pages and --since can be used only with --all or a four-digit card ending".into(),
        );
    }
    let action = action.unwrap_or(Action::Help);
    Ok(Parsed {
        action,
        config,
        pages,
        since,
    })
}

fn set_action(slot: &mut Option<Action>, action: Action) -> Result<(), String> {
    if matches!(slot, Some(Action::Help | Action::Version)) {
        return Ok(());
    }
    if slot.is_some() {
        return Err(
            "actions are mutually exclusive; use --all, --cards, or one four-digit card ending"
                .into(),
        );
    }
    *slot = Some(action);
    Ok(())
}
