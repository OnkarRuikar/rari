use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;
use std::path::Path;

use indexmap::IndexMap;
use rari_utils::io::read_to_string;
use schemars::JsonSchema;
use serde::de::{self, value, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use url::Url;

use crate::error::Error;

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct WebFeatures {
    pub features: IndexMap<String, FeatureData>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct DirtyWebFeatures {
    pub features: IndexMap<String, Value>,
}

impl WebFeatures {
    pub fn from_file(path: &Path) -> Result<Self, Error> {
        let json_str = read_to_string(path)?;
        let dirty_map: DirtyWebFeatures = serde_json::from_str(&json_str)?;
        let map = WebFeatures {
            features: dirty_map
                .features
                .into_iter()
                .filter_map(|(k, v)| {
                    serde_json::from_value::<FeatureData>(v)
                        .inspect_err(|e| {
                            tracing::error!("Error serializing baseline for {}: {}", k, &e)
                        })
                        .ok()
                        .map(|v| (k, v))
                })
                .collect(),
        };
        Ok(map)
    }

    pub fn feature_status(&self, bcd_key: &str) -> Option<&SupportStatusWithByKey> {
        self.features.values().find_map(|feature_data| {
            if let Some(ref status) = feature_data.status {
                if feature_data
                    .compat_features
                    .iter()
                    .any(|key| key == bcd_key)
                {
                    if feature_data.discouraged.is_some() {
                        return None
                    }
                    if let Some(by_key) = &status.by_compat_key {
                        if let Some(key_status) = by_key.get(bcd_key) {
                            if key_status.baseline == status.baseline {
                                return Some(status);
                            }
                        }
                    }
                }
            }
            None
        })
    }
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct FeatureData {
    /** Specification */
    #[serde(
        deserialize_with = "t_or_vec",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub spec: Vec<Url>,
    /** caniuse.com identifier */
    #[serde(
        deserialize_with = "t_or_vec",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub caniuse: Vec<String>,
    /** Whether a feature is considered a "baseline" web platform feature and when it achieved that status */
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<SupportStatusWithByKey>,
    /** Sources of support data for this feature */
    #[serde(
        deserialize_with = "t_or_vec",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub compat_features: Vec<String>,
    pub description: String,
    pub description_html: String,
    #[serde(
        deserialize_with = "t_or_vec",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub group: Vec<String>,
    pub name: String,
    #[serde(
        deserialize_with = "t_or_vec",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub snapshot: Vec<String>,
    /** Whether developers are formally discouraged from using this feature */
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discouraged: Option<Value>,
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum BrowserIdentifier {
    Chrome,
    ChromeAndroid,
    Edge,
    Firefox,
    FirefoxAndroid,
    Safari,
    SafariIos,
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BaselineHighLow {
    High,
    Low,
    #[serde(untagged)]
    False(bool),
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct SupportStatus {
    /// Whether the feature is Baseline (low substatus), Baseline (high substatus), or not (false)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline: Option<BaselineHighLow>,
    /// Date the feature achieved Baseline low status
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_low_date: Option<String>,
    /// Date the feature achieved Baseline high status
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_high_date: Option<String>,
    /// Browser versions that most-recently introduced the feature
    pub support: BTreeMap<BrowserIdentifier, String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct SupportStatusWithByKey {
    /// Whether the feature is Baseline (low substatus), Baseline (high substatus), or not (false)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline: Option<BaselineHighLow>,
    /// Date the feature achieved Baseline low status
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_low_date: Option<String>,
    /// Date the feature achieved Baseline high status
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_high_date: Option<String>,
    /// Browser versions that most-recently introduced the feature
    pub support: BTreeMap<BrowserIdentifier, String>,
    #[serde(default, skip_serializing)]
    pub by_compat_key: Option<BTreeMap<String, SupportStatus>>,
}

pub fn t_or_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct TOrVec<T>(PhantomData<T>);

    impl<'de, T> Visitor<'de> for TOrVec<T>
    where
        T: Deserialize<'de>,
    {
        type Value = Vec<T>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("string or list of strings")
        }

        fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(vec![Deserialize::deserialize(
                value::StrDeserializer::new(s),
            )?])
        }

        fn visit_seq<S>(self, seq: S) -> Result<Self::Value, S::Error>
        where
            S: SeqAccess<'de>,
        {
            Deserialize::deserialize(value::SeqAccessDeserializer::new(seq))
        }
    }

    deserializer.deserialize_any(TOrVec::<T>(PhantomData))
}
