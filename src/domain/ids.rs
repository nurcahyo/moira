use std::{fmt, ops::Deref, str::FromStr};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::AppError;

macro_rules! uuid_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new_v7() -> Self {
                Self(Uuid::now_v7())
            }

            pub fn from_uuid(id: Uuid) -> Self {
                Self(id)
            }

            pub fn into_uuid(self) -> Uuid {
                self.0
            }
        }

        impl Deref for $name {
            type Target = Uuid;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl From<Uuid> for $name {
            fn from(value: Uuid) -> Self {
                Self(value)
            }
        }

        impl From<$name> for Uuid {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(value).map(Self)
            }
        }
    };
}

macro_rules! bounded_string {
    ($name:ident, $max:expr, $label:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn parse(value: impl Into<String>) -> Result<Self, AppError> {
                let value = value.into();
                validate_opaque_id($label, &value, $max)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl Deref for $name {
            type Target = str;

            fn deref(&self) -> &Self::Target {
                self.0.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl TryFrom<String> for $name {
            type Error = AppError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::parse(value)
            }
        }
    };
}

uuid_id!(ApplicationId);
uuid_id!(ProviderId);
uuid_id!(ProviderModelId);
uuid_id!(ProviderCredentialId);
uuid_id!(SystemKeyId);
uuid_id!(ConsumerKeyId);
uuid_id!(TrustedJwtIssuerId);
uuid_id!(AuditEventId);
uuid_id!(RequestId);
uuid_id!(ExecutionId);
uuid_id!(AttemptId);
uuid_id!(RouteId);
uuid_id!(RoutingPolicyId);
uuid_id!(AgentProfileId);

bounded_string!(ExternalUserId, 256, "external_user_id");
bounded_string!(ExternalTenantId, 256, "external_tenant_id");
bounded_string!(ExternalApplicationId, 256, "external_application_id");

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ApplicationSlug(String);

impl ApplicationSlug {
    pub fn parse(value: impl Into<String>) -> Result<Self, AppError> {
        let value = value.into();
        if value.is_empty() {
            return Err(AppError::BadRequest(
                "application_slug must not be empty".to_string(),
            ));
        }
        if value.len() > 128 {
            return Err(AppError::BadRequest(
                "application_slug must be at most 128 characters".to_string(),
            ));
        }
        if !value.is_ascii() || value.chars().any(|ch| ch.is_ascii_control()) {
            return Err(AppError::BadRequest(
                "application_slug must contain printable ASCII characters".to_string(),
            ));
        }
        let mut chars = value.chars();
        let Some(first) = chars.next() else {
            unreachable!("empty checked above");
        };
        let last = value.chars().next_back().unwrap_or(first);
        if !first.is_ascii_alphanumeric() || !last.is_ascii_alphanumeric() {
            return Err(AppError::BadRequest(
                "application_slug must start and end with an alphanumeric character".to_string(),
            ));
        }
        if value
            .chars()
            .any(|ch| !(ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_'))
        {
            return Err(AppError::BadRequest(
                "application_slug may contain only lowercase ASCII letters, digits, hyphen, or underscore"
                    .to_string(),
            ));
        }

        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl Deref for ApplicationSlug {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.0.as_str()
    }
}

impl fmt::Display for ApplicationSlug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

fn validate_opaque_id(label: &str, value: &str, max_len: usize) -> Result<(), AppError> {
    if value.is_empty() {
        return Err(AppError::BadRequest(format!("{label} must not be empty")));
    }
    if value.len() > max_len {
        return Err(AppError::BadRequest(format!(
            "{label} must be at most {max_len} characters"
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(AppError::BadRequest(format!(
            "{label} must not contain control characters"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_ids_are_opaque_but_bounded() {
        assert!(ExternalUserId::parse("User:123/alpha").is_ok());
        assert!(ExternalTenantId::parse("").is_err());
        assert!(ExternalApplicationId::parse("bad\nid").is_err());
    }

    #[test]
    fn application_slug_rules_are_specific() {
        assert!(ApplicationSlug::parse("app_1-prod").is_ok());
        assert!(ApplicationSlug::parse("App").is_err());
        assert!(ApplicationSlug::parse("-app").is_err());
        assert!(ApplicationSlug::parse("app-").is_err());
    }

    #[test]
    fn internal_ids_are_uuid_v7() {
        let id = ApplicationId::new_v7().into_uuid();
        assert_eq!(id.get_version_num(), 7);
    }
}
