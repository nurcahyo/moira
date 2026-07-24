use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use utoipa::ToSchema;

pub type ResponseTextArgs = Value;

fn empty_message_args() -> Value {
    Value::Object(Map::new())
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ResponseText {
    pub message_key: String,
    pub message: String,
    #[serde(default = "empty_message_args")]
    pub message_args: ResponseTextArgs,
}

impl ResponseText {
    pub fn new(message_key: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            message_key: message_key.into(),
            message: message.into(),
            message_args: empty_message_args(),
        }
    }

    pub fn with_args(
        message_key: impl Into<String>,
        message: impl Into<String>,
        message_args: ResponseTextArgs,
    ) -> Self {
        Self {
            message_key: message_key.into(),
            message: message.into(),
            message_args,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_text_defaults_to_empty_message_args() {
        let text = ResponseText::new("moira.notice.ready", "Ready");

        assert_eq!(text.message_key, "moira.notice.ready");
        assert_eq!(text.message, "Ready");
        assert_eq!(text.message_args.as_object().unwrap().len(), 0);
    }
}
