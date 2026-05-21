use crate::hap::runtime::{CharacteristicEvent, Subscriptions};

pub(super) fn build_event_body(
    changes: &[CharacteristicEvent],
    subs: &Subscriptions,
) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for event in changes {
        if subs.contains(&event.id) {
            out.push(serde_json::json!({
                "aid": event.id.aid.0,
                "iid": event.id.iid.0,
                "value": event.value.clone(),
            }));
        }
    }
    if out.is_empty() {
        return None;
    }
    Some(
        serde_json::json!({ "characteristics": out })
            .to_string()
            .into_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hap::runtime::CharacteristicId;
    use serde_json::json;

    #[test]
    fn event_body_filters_to_subscribed_characteristics() {
        let event = CharacteristicEvent {
            id: CharacteristicId::new(2, 9),
            value: json!(100),
        };
        let mut subs = Subscriptions::default();
        assert!(build_event_body(std::slice::from_ref(&event), &subs).is_none());

        subs.insert(event.id);
        let body = build_event_body(&[event], &subs).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["characteristics"][0]["aid"], 2);
        assert_eq!(parsed["characteristics"][0]["iid"], 9);
        assert_eq!(parsed["characteristics"][0]["value"], 100);
    }
}
