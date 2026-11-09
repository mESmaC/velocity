//! Decimal JSON strings preserve every native identifier, including 64-bit usize.

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};

pub fn serialize<S: Serializer>(number: &usize, serializer: S) -> Result<S::Ok, S::Error> {
    number.to_string().serialize(serializer)
}

pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<usize, D::Error> {
    String::deserialize(deserializer)?
        .parse()
        .map_err(D::Error::custom)
}

pub mod optional {
    use super::*;

    pub fn serialize<S: Serializer>(
        number: &Option<usize>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        number
            .map(|number| number.to_string())
            .serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<usize>, D::Error> {
        Option::<String>::deserialize(deserializer)?
            .map(|number| number.parse().map_err(D::Error::custom))
            .transpose()
    }
}
