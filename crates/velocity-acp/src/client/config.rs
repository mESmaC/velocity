//! Running a session on the chosen model, effort and speed (F15), and reading what an agent
//! offers: its models and which of them have a fast mode. Decisions come from
//! `session_config`; this module only speaks ACP.

use std::sync::Arc;

use agent_client_protocol::Agent;
use agent_client_protocol::v1::ConnectionTo;
use agent_client_protocol::v1::schema::{
    SessionConfigId, SessionConfigKind, SessionConfigOption, SessionConfigOptionCategory,
    SessionConfigOptionValue, SessionConfigSelectOptions, SessionConfigValueId, SessionId,
    SetSessionConfigOptionRequest,
};

use crate::RunSettings;
use crate::session_config::{
    ConfigCategory, ConfigChoices, ConfigOption, ConfigValue, choose_config_values,
};

/// Sets one option at a time: choosing a model can change which efforts and speeds the
/// agent offers, so each answer's options decide the next change.
pub(super) async fn apply_settings(
    connection: &ConnectionTo<Agent>,
    session_id: &SessionId,
    mut options: Vec<SessionConfigOption>,
    settings: &RunSettings<String>,
) -> Result<Vec<SessionConfigOption>, agent_client_protocol::Error> {
    // Each change settles one option, so there are never more rounds than options.
    for _ in 0..=options.len() {
        let views = option_views(&options);
        let chosen = choose_config_values(&views, &settings.model, settings.effort, settings.speed);
        let Some((id, value)) = chosen.into_iter().next() else {
            return Ok(options);
        };
        options = set_option(connection, session_id, id, value).await?;
    }
    Ok(options)
}

/// `session/set_config_option`, answering the agent's options after the change.
pub(super) async fn set_option(
    connection: &ConnectionTo<Agent>,
    session_id: &SessionId,
    id: Arc<str>,
    value: ConfigValue<Arc<str>>,
) -> Result<Vec<SessionConfigOption>, agent_client_protocol::Error> {
    let value = match value {
        ConfigValue::Select(value) => {
            SessionConfigOptionValue::value_id(SessionConfigValueId::new(value))
        }
        ConfigValue::Boolean(value) => SessionConfigOptionValue::boolean(value),
    };
    let request =
        SetSessionConfigOptionRequest::new(session_id.clone(), SessionConfigId::new(id), value);
    Ok(connection
        .send_request(request)
        .block_task()
        .await?
        .config_options)
}

pub(super) fn option_views(options: &[SessionConfigOption]) -> Vec<ConfigOption<Arc<str>>> {
    options.iter().filter_map(option_view).collect()
}

/// The pure view of an ACP option. Kinds this build doesn't know are skipped.
fn option_view(option: &SessionConfigOption) -> Option<ConfigOption<Arc<str>>> {
    let category = match option.category {
        Some(SessionConfigOptionCategory::Model) => ConfigCategory::Model,
        Some(SessionConfigOptionCategory::ThoughtLevel) => ConfigCategory::ThoughtLevel,
        _ => ConfigCategory::Other,
    };
    let choices = match &option.kind {
        SessionConfigKind::Select(select) => ConfigChoices::Select {
            current: Arc::clone(&select.current_value.0),
            values: select_values(&select.options),
        },
        SessionConfigKind::Boolean(boolean) => ConfigChoices::Boolean {
            current: boolean.current_value,
        },
        _ => return None,
    };
    Some(ConfigOption::new(
        Arc::clone(&option.id.0),
        Arc::from(option.name.as_str()),
        category,
        choices,
    ))
}

fn select_values(options: &SessionConfigSelectOptions) -> Vec<(Arc<str>, Arc<str>)> {
    let pair = |value: &agent_client_protocol::v1::schema::SessionConfigSelectOption| {
        (Arc::clone(&value.value.0), Arc::from(value.name.as_str()))
    };
    match options {
        SessionConfigSelectOptions::Ungrouped(values) => values.iter().map(pair).collect(),
        SessionConfigSelectOptions::Grouped(groups) => groups
            .iter()
            .flat_map(|group| group.options.iter().map(pair))
            .collect(),
        _ => Vec::new(),
    }
}
