//! Which of an agent's session config options to set so a session runs on the chosen model,
//! effort and speed (F15). Agents advertise these as ACP config options on `session/new`;
//! Velocity matches them by category and name, never by a fixed per-provider list.

use crate::{Effort, Speed};

/// What an option is for, from its ACP category.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigCategory {
    Model,
    ThoughtLevel,
    Other,
}

/// The values an option takes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigChoices<Text> {
    /// A dropdown: the current value id, then each value as (id, display name).
    Select {
        current: Text,
        values: Vec<(Text, Text)>,
    },
    Boolean {
        current: bool,
    },
}

/// One advertised option.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigOption<Text> {
    pub id: Text,
    pub name: Text,
    pub category: ConfigCategory,
    pub choices: ConfigChoices<Text>,
}

impl<Text> ConfigOption<Text> {
    pub fn new(
        id: Text,
        name: Text,
        category: ConfigCategory,
        choices: ConfigChoices<Text>,
    ) -> Self {
        ConfigOption {
            id,
            name,
            category,
            choices,
        }
    }
}

/// A value to send with `session/set_config_option`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigValue<Text> {
    Select(Text),
    Boolean(bool),
}

/// The (option id, value) pairs to set, in advertised order. An option is set only when it
/// matches and its current value differs. A model or effort the agent doesn't list is left at
/// the agent's own default rather than failing the session.
pub fn choose_config_values<Text: AsRef<str> + Clone>(
    options: &[ConfigOption<Text>],
    model: &str,
    effort: Effort,
    speed: Speed,
) -> Vec<(Text, ConfigValue<Text>)> {
    options
        .iter()
        .filter_map(|option| {
            let wanted = wanted_value(option, model, effort, speed)?;
            (!is_current(&option.choices, &wanted)).then(|| (option.id.clone(), wanted))
        })
        .collect()
}

fn wanted_value<Text: AsRef<str> + Clone>(
    option: &ConfigOption<Text>,
    model: &str,
    effort: Effort,
    speed: Speed,
) -> Option<ConfigValue<Text>> {
    match (&option.category, &option.choices) {
        (ConfigCategory::Model, ConfigChoices::Select { values, .. }) => {
            select_matching(values, &[model])
        }
        (ConfigCategory::ThoughtLevel, ConfigChoices::Select { values, .. }) => {
            select_matching(values, effort_names(effort))
        }
        (ConfigCategory::Other, ConfigChoices::Boolean { .. }) if is_fast_option(option) => {
            Some(ConfigValue::Boolean(speed == Speed::Fast))
        }
        (ConfigCategory::Other, ConfigChoices::Select { values, .. })
            if is_speed_option(option) =>
        {
            select_matching(values, speed_names(speed))
        }
        _ => None,
    }
}

/// The first value whose id or name matches any of `names`, ignoring case, spaces and
/// punctuation ("GPT-6 Luna" matches `gpt-6-luna`).
fn select_matching<Text: AsRef<str> + Clone>(
    values: &[(Text, Text)],
    names: &[&str],
) -> Option<ConfigValue<Text>> {
    names.iter().find_map(|name| {
        let wanted = normalized(name);
        values
            .iter()
            .find(|(id, label)| {
                normalized(id.as_ref()) == wanted || normalized(label.as_ref()) == wanted
            })
            .map(|(id, _)| ConfigValue::Select(id.clone()))
    })
}

fn effort_names(effort: Effort) -> &'static [&'static str] {
    match effort {
        Effort::Low => &["low", "minimal"],
        Effort::Medium => &["medium"],
        Effort::High => &["high"],
        Effort::Max => &["max", "xhigh", "extra high"],
    }
}

fn speed_names(speed: Speed) -> &'static [&'static str] {
    match speed {
        Speed::Fast => &["fast"],
        Speed::Regular => &["regular", "standard", "normal", "default"],
    }
}

fn is_fast_option<Text: AsRef<str>>(option: &ConfigOption<Text>) -> bool {
    [option.id.as_ref(), option.name.as_ref()]
        .iter()
        .any(|text| normalized(text).contains("fast"))
}

fn is_speed_option<Text: AsRef<str>>(option: &ConfigOption<Text>) -> bool {
    [option.id.as_ref(), option.name.as_ref()]
        .iter()
        .any(|text| normalized(text).contains("speed") || normalized(text).contains("fast"))
}

fn is_current<Text: AsRef<str>>(choices: &ConfigChoices<Text>, wanted: &ConfigValue<Text>) -> bool {
    match (choices, wanted) {
        (ConfigChoices::Select { current, .. }, ConfigValue::Select(value)) => {
            current.as_ref() == value.as_ref()
        }
        (ConfigChoices::Boolean { current }, ConfigValue::Boolean(value)) => current == value,
        _ => false,
    }
}

fn normalized(text: &str) -> String {
    text.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
