//! JSON-RPC notification builders — the `(method, params)` shape of each
//! control notification, used by `main.rs` when it fans an internal
//! `ServerEvent` out to the control clients. Note the field-name trap these
//! pin down: `Client.OnVolumeChanged` uses `muted`, `Group.OnMute` uses `mute`.

use serde_json::{Value, json};

fn notification(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}

/// `Client.OnVolumeChanged` — a client's volume/mute changed.
pub(crate) fn client_on_volume_changed(client_id: &str, percent: u16, muted: bool) -> Value {
    notification(
        "Client.OnVolumeChanged",
        json!({"id": client_id, "volume": {"percent": percent, "muted": muted}}),
    )
}

/// `Client.OnLatencyChanged` — a client's latency changed.
pub(crate) fn client_on_latency_changed(client_id: &str, latency: i32) -> Value {
    notification(
        "Client.OnLatencyChanged",
        json!({"id": client_id, "latency": latency}),
    )
}

/// `Client.OnNameChanged` — a client was renamed.
pub(crate) fn client_on_name_changed(client_id: &str, name: &str) -> Value {
    notification(
        "Client.OnNameChanged",
        json!({"id": client_id, "name": name}),
    )
}

/// `Group.OnMute` — a group's mute state changed.
pub(crate) fn group_on_mute(group_id: &str, muted: bool) -> Value {
    notification("Group.OnMute", json!({"id": group_id, "mute": muted}))
}

/// `Group.OnStreamChanged` — a group's stream assignment changed.
pub(crate) fn group_on_stream_changed(group_id: &str, stream_id: &str) -> Value {
    notification(
        "Group.OnStreamChanged",
        json!({"id": group_id, "stream_id": stream_id}),
    )
}

/// `Group.OnNameChanged` — a group was renamed.
pub(crate) fn group_on_name_changed(group_id: &str, name: &str) -> Value {
    notification("Group.OnNameChanged", json!({"id": group_id, "name": name}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_uses_muted_not_mute() {
        let n = client_on_volume_changed("c1", 80, true);
        assert_eq!(n["method"], "Client.OnVolumeChanged");
        assert_eq!(n["params"]["volume"]["percent"], 80);
        assert_eq!(n["params"]["volume"]["muted"], true);
    }

    #[test]
    fn group_mute_uses_mute_key() {
        // Group.OnMute deliberately uses "mute" (not "muted").
        let n = group_on_mute("g1", true);
        assert_eq!(n["params"]["mute"], true);
        assert!(n["params"]["muted"].is_null());
    }

    // ---- Envelope invariant -------------------------------------------------

    /// Every builder must produce a JSON-RPC 2.0 *notification* envelope:
    /// top-level `jsonrpc: "2.0"`, top-level `method`, top-level `params`, and
    /// crucially NO `id` (notifications are fire-and-forget, unlike responses).
    fn assert_notification_envelope(n: &Value, expected_method: &str) {
        assert_eq!(n["jsonrpc"], "2.0", "jsonrpc version must be 2.0");
        assert_eq!(n["method"], expected_method, "method must match");
        assert!(n["params"].is_object(), "params must be an object");
        assert!(
            n.get("id").is_none(),
            "a notification must NOT carry an id field"
        );
        // Envelope has exactly three keys: jsonrpc, method, params.
        let obj = n.as_object().expect("envelope must be a JSON object");
        assert_eq!(obj.len(), 3, "envelope must have exactly 3 top-level keys");
    }

    #[test]
    fn all_builders_produce_valid_jsonrpc_notification_envelope() {
        assert_notification_envelope(
            &client_on_volume_changed("c1", 50, false),
            "Client.OnVolumeChanged",
        );
        assert_notification_envelope(
            &client_on_latency_changed("c1", 10),
            "Client.OnLatencyChanged",
        );
        assert_notification_envelope(
            &client_on_name_changed("c1", "Kitchen"),
            "Client.OnNameChanged",
        );
        assert_notification_envelope(&group_on_mute("g1", false), "Group.OnMute");
        assert_notification_envelope(
            &group_on_stream_changed("g1", "spotify"),
            "Group.OnStreamChanged",
        );
        assert_notification_envelope(
            &group_on_name_changed("g1", "Living Room"),
            "Group.OnNameChanged",
        );
    }

    // ---- client_on_volume_changed ------------------------------------------

    #[test]
    fn volume_changed_full_shape() {
        let n = client_on_volume_changed("living-room", 80, true);
        assert_eq!(n["method"], "Client.OnVolumeChanged");
        assert_eq!(n["params"]["id"], "living-room");
        // volume is a nested object with percent + muted (note: "muted" here).
        assert_eq!(n["params"]["volume"]["percent"], 80);
        assert_eq!(n["params"]["volume"]["muted"], true);
        // params has exactly id + volume; volume has exactly percent + muted.
        assert_eq!(n["params"].as_object().unwrap().len(), 2);
        assert_eq!(n["params"]["volume"].as_object().unwrap().len(), 2);
    }

    #[test]
    fn volume_changed_unmuted_false() {
        let n = client_on_volume_changed("c1", 0, false);
        assert_eq!(n["params"]["volume"]["muted"], false);
        // percent 0 must serialize as the integer 0, not null / absent.
        assert_eq!(n["params"]["volume"]["percent"], 0);
        assert!(n["params"]["volume"]["percent"].is_u64());
    }

    #[test]
    fn volume_changed_percent_boundaries() {
        // Boundary values: 100 (max valid) and u16::MAX (unclamped by builder —
        // the builder is a pure formatter and does not range-check).
        let hundred = client_on_volume_changed("c1", 100, false);
        assert_eq!(hundred["params"]["volume"]["percent"], 100);

        let maxed = client_on_volume_changed("c1", u16::MAX, false);
        assert_eq!(maxed["params"]["volume"]["percent"], u16::MAX);
    }

    #[test]
    fn volume_changed_preserves_client_id_verbatim() {
        // IDs are opaque strings; special characters must pass through untouched.
        let id = "client/with:weird.id-42";
        let n = client_on_volume_changed(id, 25, false);
        assert_eq!(n["params"]["id"], id);
    }

    // ---- client_on_latency_changed -----------------------------------------

    #[test]
    fn latency_changed_full_shape() {
        let n = client_on_latency_changed("c2", 100);
        assert_eq!(n["method"], "Client.OnLatencyChanged");
        assert_eq!(n["params"]["id"], "c2");
        assert_eq!(n["params"]["latency"], 100);
        assert_eq!(n["params"].as_object().unwrap().len(), 2);
    }

    #[test]
    fn latency_changed_negative_and_zero() {
        // latency is i32 — negative values are legal and must serialize signed.
        let neg = client_on_latency_changed("c1", -100);
        assert_eq!(neg["params"]["latency"], -100);
        assert!(neg["params"]["latency"].is_i64());

        let zero = client_on_latency_changed("c1", 0);
        assert_eq!(zero["params"]["latency"], 0);
    }

    #[test]
    fn latency_changed_i32_extremes() {
        let max = client_on_latency_changed("c1", i32::MAX);
        assert_eq!(max["params"]["latency"], i32::MAX);

        let min = client_on_latency_changed("c1", i32::MIN);
        assert_eq!(min["params"]["latency"], i32::MIN);
    }

    // ---- client_on_name_changed --------------------------------------------

    #[test]
    fn name_changed_full_shape() {
        let n = client_on_name_changed("c3", "Kitchen");
        assert_eq!(n["method"], "Client.OnNameChanged");
        assert_eq!(n["params"]["id"], "c3");
        assert_eq!(n["params"]["name"], "Kitchen");
        assert_eq!(n["params"].as_object().unwrap().len(), 2);
    }

    #[test]
    fn name_changed_empty_and_unicode_names() {
        // Empty name (a client can be un-named) must serialize as "" not null.
        let empty = client_on_name_changed("c1", "");
        assert_eq!(empty["params"]["name"], "");
        assert!(empty["params"]["name"].is_string());

        // Unicode / whitespace names must round-trip verbatim.
        let unicode = client_on_name_changed("c1", "Küche 🔊");
        assert_eq!(unicode["params"]["name"], "Küche 🔊");
    }

    // ---- group_on_mute ------------------------------------------------------

    #[test]
    fn group_mute_full_shape() {
        let n = group_on_mute("g1", true);
        assert_eq!(n["method"], "Group.OnMute");
        assert_eq!(n["params"]["id"], "g1");
        assert_eq!(n["params"]["mute"], true);
        assert_eq!(n["params"].as_object().unwrap().len(), 2);
    }

    #[test]
    fn group_mute_false_still_uses_mute_key() {
        // Guard the "mute" vs "muted" trap for the false branch too.
        let n = group_on_mute("g1", false);
        assert_eq!(n["params"]["mute"], false);
        assert!(n["params"]["muted"].is_null());
        assert!(n["params"]["mute"].is_boolean());
    }

    // ---- group_on_stream_changed -------------------------------------------

    #[test]
    fn group_stream_changed_full_shape() {
        let n = group_on_stream_changed("g2", "spotify");
        assert_eq!(n["method"], "Group.OnStreamChanged");
        assert_eq!(n["params"]["id"], "g2");
        // key is "stream_id" (snake_case), not "streamId" / "stream".
        assert_eq!(n["params"]["stream_id"], "spotify");
        assert!(n["params"]["streamId"].is_null());
        assert!(n["params"]["stream"].is_null());
        assert_eq!(n["params"].as_object().unwrap().len(), 2);
    }

    #[test]
    fn group_stream_changed_empty_stream_id() {
        // Clearing a group's stream assignment passes an empty stream id.
        let n = group_on_stream_changed("g1", "");
        assert_eq!(n["params"]["stream_id"], "");
        assert!(n["params"]["stream_id"].is_string());
    }

    // ---- group_on_name_changed ---------------------------------------------

    #[test]
    fn group_name_changed_full_shape() {
        let n = group_on_name_changed("g3", "Master");
        assert_eq!(n["method"], "Group.OnNameChanged");
        assert_eq!(n["params"]["id"], "g3");
        assert_eq!(n["params"]["name"], "Master");
        assert_eq!(n["params"].as_object().unwrap().len(), 2);
    }

    #[test]
    fn group_and_client_name_changed_have_distinct_methods() {
        // Same params shape ({id, name}) but the method strings must differ.
        let g = group_on_name_changed("x", "n");
        let c = client_on_name_changed("x", "n");
        assert_eq!(g["method"], "Group.OnNameChanged");
        assert_eq!(c["method"], "Client.OnNameChanged");
        assert_ne!(g["method"], c["method"]);
        // params are structurally identical — only the method disambiguates.
        assert_eq!(g["params"], c["params"]);
    }
}
