use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::hap::runtime::{
    CharacteristicId, CharacteristicRead, CharacteristicWrite, CharacteristicWriteOutcome,
    CharacteristicWriteStatus, HapAccessoryApp, HapStatus, Subscriptions,
};

pub(super) async fn handle_get_characteristics(
    app: &impl HapAccessoryApp,
    ids: &[CharacteristicId],
) -> Result<Vec<u8>> {
    let values = app.read_characteristics(ids).await?;
    Ok(characteristics_body(values))
}

pub(super) async fn handle_put_characteristics(
    app: &impl HapAccessoryApp,
    body: &[u8],
    subs: &mut Subscriptions,
) -> Result<CharacteristicWriteOutcome> {
    let parsed: Value = serde_json::from_slice(body)?;
    let chars = parsed
        .get("characteristics")
        .and_then(|c| c.as_array())
        .ok_or_else(|| anyhow!("missing characteristics array"))?;

    let writes = chars
        .iter()
        .map(parse_characteristic_write)
        .collect::<Vec<_>>();
    app.write_characteristics(writes, subs).await
}

pub(super) fn parse_characteristic_ids(
    ids: &str,
) -> std::result::Result<Vec<CharacteristicId>, HapStatus> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    ids.split(',')
        .map(|pair| {
            let mut parts = pair.split('.');
            let aid = parts
                .next()
                .and_then(|s| s.parse().ok())
                .ok_or(HapStatus::InvalidValueInRequest)?;
            let iid = parts
                .next()
                .and_then(|s| s.parse().ok())
                .ok_or(HapStatus::InvalidValueInRequest)?;
            if parts.next().is_some() {
                return Err(HapStatus::InvalidValueInRequest);
            }
            Ok(CharacteristicId::new(aid, iid))
        })
        .collect()
}

pub(super) fn parse_characteristic_write(entry: &Value) -> CharacteristicWrite {
    let aid = entry.get("aid").and_then(|v| v.as_u64());
    let iid = entry.get("iid").and_then(|v| v.as_u64());
    let id = match (aid, iid) {
        (Some(aid), Some(iid)) => CharacteristicId::new(aid, iid),
        _ => CharacteristicId::new(0, 0),
    };
    CharacteristicWrite {
        id,
        value: entry.get("value").cloned(),
        ev: entry.get("ev").and_then(|v| v.as_bool()),
    }
}

pub(super) fn characteristics_body(reads: Vec<CharacteristicRead>) -> Vec<u8> {
    let characteristics = reads
        .into_iter()
        .map(|read| {
            if read.status == HapStatus::Success {
                serde_json::json!({
                    "aid": read.id.aid.0,
                    "iid": read.id.iid.0,
                    "value": read.value,
                })
            } else {
                serde_json::json!({
                    "aid": read.id.aid.0,
                    "iid": read.id.iid.0,
                    "status": read.status.code(),
                })
            }
        })
        .collect::<Vec<_>>();
    serde_json::json!({ "characteristics": characteristics })
        .to_string()
        .into_bytes()
}

pub(super) fn write_statuses_body(statuses: Vec<CharacteristicWriteStatus>) -> Vec<u8> {
    let characteristics = statuses
        .into_iter()
        .map(|status| {
            serde_json::json!({
                "aid": status.id.aid.0,
                "iid": status.id.iid.0,
                "status": status.status.code(),
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({ "characteristics": characteristics })
        .to_string()
        .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hap::runtime::{CharacteristicWriteStatus, HapStatus};
    use serde_json::json;

    #[test]
    fn parses_characteristic_ids() {
        let ids = parse_characteristic_ids("2.9,3.10").unwrap();
        assert_eq!(ids[0], CharacteristicId::new(2, 9));
        assert_eq!(ids[1], CharacteristicId::new(3, 10));
    }

    #[test]
    fn malformed_characteristic_ids_return_invalid_value() {
        for ids in ["bad", "2", "2.bad", "2.9.extra", "2.9,"] {
            assert_eq!(
                parse_characteristic_ids(ids),
                Err(HapStatus::InvalidValueInRequest),
                "{ids}"
            );
        }
    }

    #[test]
    fn missing_aid_or_iid_maps_to_invalid_write() {
        let write = parse_characteristic_write(&json!({"value": 50}));
        assert_eq!(write.id, CharacteristicId::new(0, 0));
    }

    #[test]
    fn characteristics_body_uses_status_for_read_errors() {
        let body = characteristics_body(vec![CharacteristicRead::error(
            CharacteristicId::new(2, 99),
            HapStatus::ResourceDoesNotExist,
        )]);
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(parsed["characteristics"][0]["aid"], 2);
        assert_eq!(parsed["characteristics"][0]["iid"], 99);
        assert_eq!(
            parsed["characteristics"][0]["status"],
            HapStatus::ResourceDoesNotExist.code()
        );
        assert!(parsed["characteristics"][0].get("value").is_none());
    }

    #[test]
    fn write_statuses_body_reports_per_characteristic_status() {
        let body = write_statuses_body(vec![CharacteristicWriteStatus::error(
            CharacteristicId::new(2, 9),
            HapStatus::ReadOnly,
        )]);
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(parsed["characteristics"][0]["aid"], 2);
        assert_eq!(parsed["characteristics"][0]["iid"], 9);
        assert_eq!(
            parsed["characteristics"][0]["status"],
            HapStatus::ReadOnly.code()
        );
    }
}
