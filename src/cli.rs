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
}

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
    let action = action.unwrap_or(Action::Help);
    Ok(Parsed { action, config })
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
