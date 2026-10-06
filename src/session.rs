use crate::dest::{TokenRequest, TokenUsage};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Session {
    pub thread_id: Option<String>,
    pub name: Option<String>,
    pub preview: String,
    pub cwd: Option<String>,
    #[serde(default)]
    pub model_provider: Option<String>,
    #[serde(default)]
    pub cost_revision: u64,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub token_usage: Option<TokenUsage>,
    /// Transient response snapshots for the monitoring run; no historical session prices.
    #[serde(default)]
    pub requests: Vec<TokenRequest>,
    pub error: Option<String>,
}

impl Session {
    #[cfg(test)]
    pub fn label(&self) -> &str {
        if let Some(error) = &self.error {
            return error;
        }
        self.name
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .or_else(|| (!self.preview.trim().is_empty()).then_some(self.preview.as_str()))
            .unwrap_or(if self.thread_id.is_some() {
                "(unnamed)"
            } else {
                "(connecting)"
            })
    }
}

// Normalize Codex token totals at the host boundary, not inside destinations.
fn parse_usage(value: &Value) -> Option<TokenUsage> {
    Some(TokenUsage {
        input_tokens: value.get("inputTokens")?.as_u64()?,
        cached_input_tokens: value.get("cachedInputTokens")?.as_u64()?,
        cache_write_input_tokens: value
            .get("cacheWriteInputTokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: value.get("outputTokens")?.as_u64()?,
        reasoning_output_tokens: value.get("reasoningOutputTokens")?.as_u64()?,
    })
}

#[derive(Clone)]
struct ThreadContext {
    model: Option<String>,
    credential_profile: Option<String>,
}

#[derive(Default)]
pub struct Tracker {
    pub current: Session,
    // Only successful replies to this TUI's selection requests select a thread.
    selections: HashMap<(u64, String), u64>,
    sequence: u64,
    selected_sequence: u64,
    reads: HashMap<(u64, String), String>,
    last_usage: HashMap<String, Value>,
    seen_usage: HashMap<String, HashSet<String>>,
    monitored_threads: HashMap<String, ThreadContext>,
    request_sequence: u64,
    turn_models: HashMap<(String, String), Option<String>>,
    pending_turns: HashMap<(u64, String), (String, Option<String>)>,
    completed_turns: std::collections::HashSet<(String, String)>,
}

fn id_key(connection: u64, id: &Value) -> (u64, String) {
    (connection, id.to_string())
}

pub fn selects_visible_thread(message: &Value) -> bool {
    match message.get("method").and_then(Value::as_str) {
        Some("thread/resume" | "thread/fork") => true,
        Some("thread/start") => {
            // Codex creates hidden ephemeral feature threads for automatic titles,
            // rename suggestions and other structured work on this same connection.
            let temporary_id = message
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| id.starts_with("temporary-structured-"));
            let feature = message
                .pointer("/params/threadSource")
                .and_then(Value::as_str)
                .is_some_and(|source| source.starts_with("feature"));
            let ephemeral = message
                .pointer("/params/ephemeral")
                .and_then(Value::as_bool)
                == Some(true);
            !(temporary_id || feature && ephemeral)
        }
        _ => false,
    }
}

impl Tracker {
    pub fn disconnected(&mut self, connection: u64) {
        self.selections.retain(|(owner, _), _| *owner != connection);
        self.reads.retain(|(owner, _), _| *owner != connection);
        self.pending_turns
            .retain(|(owner, _), _| *owner != connection);
    }

    pub fn request(&mut self, connection: u64, message: &Value) {
        let Some(id) = message.get("id") else { return };
        if selects_visible_thread(message) {
            self.sequence += 1;
            self.selections
                .insert(id_key(connection, id), self.sequence);
            return;
        }
        if message.get("method").and_then(Value::as_str) == Some("turn/start")
            && let Some(thread_id) = message.pointer("/params/threadId").and_then(Value::as_str)
        {
            let model = message
                .pointer("/params/model")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    self.monitored_threads
                        .get(thread_id)
                        .and_then(|context| context.model.clone())
                });
            self.pending_turns
                .insert(id_key(connection, id), (thread_id.to_owned(), model));
        }
        if let Some("thread/read") = message.get("method").and_then(Value::as_str)
            && let Some(thread_id) = message.pointer("/params/threadId").and_then(Value::as_str)
        {
            self.reads
                .insert(id_key(connection, id), thread_id.to_owned());
        }
    }

    fn metadata(&mut self, thread: &Value) {
        self.current.name = thread
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.current.preview = thread
            .get("preview")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        self.current.cwd = thread.get("cwd").and_then(Value::as_str).map(str::to_owned);
        self.current.model_provider = thread
            .get("modelProvider")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let Some(model) = thread.get("model").and_then(Value::as_str) {
            self.current.model = Some(model.to_owned());
        }
        self.current.error = None;
    }

    pub fn response(&mut self, connection: u64, message: &Value) {
        if let Some(id) = message.get("id") {
            let key = id_key(connection, id);
            if let Some((thread_id, model)) = self.pending_turns.remove(&key)
                && let Some(turn_id) = message.pointer("/result/turn/id").and_then(Value::as_str)
            {
                self.turn_models
                    .insert((thread_id.clone(), turn_id.to_owned()), model.clone());
                if let Some(context) = self.monitored_threads.get_mut(&thread_id) {
                    context.model = model.clone();
                }
                if self.current.thread_id.as_deref() == Some(&thread_id) {
                    self.current.model = model;
                }
            }
            if let Some(sequence) = self.selections.remove(&key)
                && let Some(thread) = message.pointer("/result/thread")
                && sequence >= self.selected_sequence
                && let Some(thread_id) = thread.get("id").and_then(Value::as_str)
            {
                self.selected_sequence = sequence;
                if self.current.thread_id.as_deref() != Some(thread_id) {
                    self.current.token_usage = None;
                }
                self.current.thread_id = Some(thread_id.to_owned());
                self.metadata(thread);
                self.current.model = message
                    .pointer("/result/model")
                    .and_then(Value::as_str)
                    .or_else(|| thread.get("model").and_then(Value::as_str))
                    .map(str::to_owned);
                self.monitored_threads.insert(
                    thread_id.to_owned(),
                    ThreadContext {
                        model: self.current.model.clone(),
                        credential_profile: self.current.model_provider.clone(),
                    },
                );
                self.current.token_usage = self.last_usage.get(thread_id).and_then(parse_usage);
            }
            if let Some(requested) = self.reads.remove(&key)
                && self.current.thread_id.as_deref() == Some(&requested)
                && let Some(thread) = message.pointer("/result/thread")
                && thread.get("id").and_then(Value::as_str) == Some(&requested)
            {
                self.metadata(thread);
            }
        }
        if message.get("method").and_then(Value::as_str) == Some("thread/name/updated") {
            let thread_id = message.pointer("/params/threadId").and_then(Value::as_str);
            if thread_id.is_some() && thread_id == self.current.thread_id.as_deref() {
                self.current.name = message
                    .pointer("/params/threadName")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
        }
        let thread_id = message.pointer("/params/threadId").and_then(Value::as_str);
        if let Some(thread_id) = thread_id
            && let Some(context) = self.monitored_threads.get(thread_id).cloned()
        {
            match message.get("method").and_then(Value::as_str) {
                Some("thread/tokenUsage/updated") => {
                    if let Some(usage) = message.pointer("/params/tokenUsage/total") {
                        let total = parse_usage(usage);
                        // Connections can replay an older response after newer ones arrive.
                        // Deduplicate normalized cumulative snapshots across all connections.
                        let snapshot = total
                            .as_ref()
                            .map(|total| {
                                serde_json::to_string(total).expect("token counters serialize")
                            })
                            .unwrap_or_else(|| usage.to_string());
                        if !self
                            .seen_usage
                            .entry(thread_id.to_owned())
                            .or_default()
                            .insert(snapshot)
                        {
                            return;
                        }
                        let previous = self.last_usage.get(thread_id).and_then(parse_usage);
                        let latest = match (&total, &previous) {
                            (Some(current), Some(previous)) => {
                                current.input_tokens >= previous.input_tokens
                                    && current.cached_input_tokens >= previous.cached_input_tokens
                                    && current.cache_write_input_tokens
                                        >= previous.cache_write_input_tokens
                                    && current.output_tokens >= previous.output_tokens
                                    && current.reasoning_output_tokens
                                        >= previous.reasoning_output_tokens
                            }
                            _ => true,
                        };
                        if latest && self.current.thread_id.as_deref() == Some(thread_id) {
                            self.current.token_usage = total.clone();
                        }
                        if total.as_ref().is_some_and(|usage| usage.validate().is_ok()) {
                            // Prefer the response's own usage. A known previous total is a safe fallback.
                            let request_usage = message
                                .pointer("/params/tokenUsage/last")
                                .and_then(parse_usage)
                                .or_else(|| total.as_ref()?.delta_from(previous.as_ref()?).ok());
                            if let Some(usage) = request_usage {
                                self.request_sequence += 1;
                                let model = message
                                    .pointer("/params/turnId")
                                    .and_then(Value::as_str)
                                    .map(|turn| {
                                        self.turn_models
                                            .entry((thread_id.to_owned(), turn.to_owned()))
                                            .or_insert_with(|| context.model.clone())
                                            .clone()
                                    })
                                    .unwrap_or_else(|| context.model.clone());
                                self.current.requests.push(TokenRequest {
                                    sequence: self.request_sequence,
                                    session_id: thread_id.to_owned(),
                                    credential_profile: context.credential_profile,
                                    model,
                                    usage,
                                });
                            } else {
                                self.current.error = Some("Per-request token usage unavailable; response was not estimated".into());
                            }
                        }
                        if latest {
                            self.last_usage.insert(thread_id.to_owned(), usage.clone());
                        }
                        self.current.cost_revision += 1;
                    }
                }
                Some("turn/completed") => {
                    if let Some(turn_id) =
                        message.pointer("/params/turn/id").and_then(Value::as_str)
                        && self
                            .completed_turns
                            .insert((thread_id.to_owned(), turn_id.to_owned()))
                    {
                        // Also refresh after failed requests or final accounting has settled.
                        self.current.cost_revision += 1;
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn delayed_replays_do_not_charge_twice_or_roll_back_the_usage_baseline() {
        let mut tracker = Tracker::default();
        tracker.request(1, &json!({"id":1,"method":"thread/resume"}));
        tracker.response(
            1,
            &json!({"id":1,"result":{"model":"model-a","thread":{"id":"chat"}}}),
        );
        let event = |total, last| {
            json!({
                "method":"thread/tokenUsage/updated","params":{"threadId":"chat","turnId":"turn",
                "tokenUsage":{"total":{"inputTokens":total,"cachedInputTokens":0,"outputTokens":0,"reasoningOutputTokens":0},
                "last":{"inputTokens":last,"cachedInputTokens":0,"outputTokens":0,"reasoningOutputTokens":0}}}
            })
        };
        let first = event(100, 100);
        tracker.response(1, &first);
        tracker.response(1, &event(300, 200));
        tracker.response(2, &first);
        assert_eq!(tracker.current.requests.len(), 2);
        assert_eq!(tracker.current.cost_revision, 2);
        // A previously unseen delayed response is still charged at its own usage.
        tracker.response(2, &event(150, 50));
        assert_eq!(
            tracker.current.token_usage.as_ref().unwrap().input_tokens,
            300
        );
        let mut next = event(400, 100);
        next["params"]["tokenUsage"]
            .as_object_mut()
            .unwrap()
            .remove("last");
        tracker.response(1, &next);
        assert_eq!(
            tracker
                .current
                .requests
                .iter()
                .map(|request| request.usage.input_tokens)
                .collect::<Vec<_>>(),
            [100, 200, 50, 100]
        );
        assert_eq!(
            tracker.current.token_usage.as_ref().unwrap().input_tokens,
            400
        );
    }

    #[test]
    fn responses_capture_their_own_usage_and_model_without_repricing_history() {
        let mut tracker = Tracker::default();
        tracker.request(1, &json!({"id":1,"method":"thread/resume"}));
        tracker.response(1, &json!({"id":1,"result":{"model":"model-a", "thread":{"id":"chat","modelProvider":"relay"}}}));
        let event = |turn: &str, total: u64, last: u64| {
            json!({
            "method":"thread/tokenUsage/updated","params":{"threadId":"chat", "turnId":turn,
            "tokenUsage":{"total":{"inputTokens":total,"cachedInputTokens":0,"outputTokens":0,"reasoningOutputTokens":0},
            "last":{"inputTokens":last,"cachedInputTokens":0,"outputTokens":0,"reasoningOutputTokens":0}}}})
        };
        let first = event("a", 10_000, 100);
        tracker.response(1, &first);
        tracker.response(2, &first);
        assert_eq!(tracker.current.requests.len(), 1);
        assert_eq!(tracker.current.requests[0].usage.input_tokens, 100);
        tracker.request(
            1,
            &json!({"id":2,"method":"turn/start","params":{"threadId":"chat","model":"model-b"}}),
        );
        tracker.response(1, &json!({"id":2,"result":{"turn":{"id":"b"}}}));
        tracker.response(1, &event("b", 10_200, 200));
        tracker.response(1, &event("b", 10_500, 300));
        // A delayed response from the earlier turn retains its original model.
        tracker.response(1, &event("a", 10_550, 50));
        let requests = &tracker.current.requests;
        assert_eq!(requests.len(), 4);
        assert_eq!(
            requests
                .iter()
                .map(|r| r.model.as_deref())
                .collect::<Vec<_>>(),
            vec![
                Some("model-a"),
                Some("model-b"),
                Some("model-b"),
                Some("model-a")
            ]
        );
        assert_eq!(
            requests
                .iter()
                .map(|r| r.usage.input_tokens)
                .collect::<Vec<_>>(),
            vec![100, 200, 300, 50]
        );
        assert!(
            requests
                .iter()
                .all(|r| r.credential_profile.as_deref() == Some("relay"))
        );
        let encoded = serde_json::to_vec(&tracker.current).unwrap();
        assert_eq!(
            serde_json::from_slice::<Session>(&encoded).unwrap(),
            tracker.current
        );

        tracker.request(1, &json!({"id":3,"method":"thread/start"}));
        tracker.response(
            1,
            &json!({"id":3,"result":{"model":"model-c","thread":{"id":"new"}}}),
        );
        assert!(tracker.current.token_usage.is_none());
        tracker.response(1, &event("a", 10_600, 50));
        assert_eq!(
            tracker.current.requests.last().unwrap().model.as_deref(),
            Some("model-a")
        );
        assert!(tracker.current.token_usage.is_none());
        let mut without_last = event("c", 50_000, 500);
        without_last["params"]["threadId"] = json!("new");
        without_last["params"]["tokenUsage"]
            .as_object_mut()
            .unwrap()
            .remove("last");
        tracker.response(1, &without_last);
        assert_eq!(tracker.current.requests.len(), 5);
        assert!(
            tracker
                .current
                .error
                .as_deref()
                .unwrap()
                .contains("not estimated")
        );
    }

    #[test]
    fn request_accounting_triggers_refresh_but_replayed_and_other_thread_events_do_not() {
        let mut t = Tracker::default();
        t.request(1, &json!({"id":1,"method":"thread/start"}));
        t.response(1, &json!({"id":1,"result":{"thread":{"id":"chat"}}}));
        let event = json!({"method":"thread/tokenUsage/updated","params":{
            "threadId":"chat","tokenUsage":{"total":{"totalTokens":123}}}});
        t.response(1, &event);
        assert_eq!(t.current.cost_revision, 1);
        t.response(2, &event);
        assert_eq!(t.current.cost_revision, 1);
        t.response(
            2,
            &json!({"method":"thread/tokenUsage/updated","params":{
            "threadId":"hidden","tokenUsage":{"total":{"totalTokens":999}}}}),
        );
        assert_eq!(t.current.cost_revision, 1);
        t.response(
            1,
            &json!({"method":"thread/tokenUsage/updated","params":{
            "threadId":"chat","tokenUsage":{"total":{"totalTokens":234}}}}),
        );
        assert_eq!(t.current.cost_revision, 2);
        let completion =
            json!({"method":"turn/completed","params":{"threadId":"chat","turn":{"id":"turn-1"}}});
        t.response(1, &completion);
        t.response(2, &completion);
        assert_eq!(t.current.cost_revision, 3);
    }

    #[test]
    fn automatic_title_thread_does_not_replace_the_visible_chat() {
        let mut tracker = Tracker::default();
        tracker.request(1, &json!({"id":1,"method":"thread/start","params":{}}));
        tracker.response(
            1,
            &json!({"id":1,"result":{"thread":{"id":"chat","name":null,"preview":""}}}),
        );
        assert_eq!(tracker.current.label(), "(unnamed)");

        let hidden = json!({"id":"temporary-structured-title","method":"thread/start",
            "params":{"ephemeral":true,"threadSource":"feature:thread_title"}});
        assert!(!selects_visible_thread(&hidden));
        tracker.request(1, &hidden);
        tracker.response(
            1,
            &json!({"id":"temporary-structured-title","result":{
            "thread":{"id":"hidden-title-worker","ephemeral":true,"name":null,"preview":""}}}),
        );
        assert_eq!(tracker.current.thread_id.as_deref(), Some("chat"));

        tracker.response(
            1,
            &json!({"method":"thread/name/updated",
            "params":{"threadId":"chat","threadName":"自动生成的标题"}}),
        );
        assert_eq!(tracker.current.label(), "自动生成的标题");
        tracker.request(
            1,
            &json!({"id":2,"method":"thread/read","params":{"threadId":"chat"}}),
        );
        tracker.response(
            1,
            &json!({"id":2,"result":{"thread":{"id":"chat","name":"自动生成的标题"}}}),
        );
        assert_eq!(tracker.current.thread_id.as_deref(), Some("chat"));
        assert_eq!(tracker.current.label(), "自动生成的标题");

        // User-visible ephemeral chats still count as selections.
        assert!(selects_visible_thread(
            &json!({"id":3,"method":"thread/start","params":{"ephemeral":true}})
        ));
        assert!(!selects_visible_thread(
            &json!({"id":4,"method":"thread/start",
            "params":{"ephemeral":true,"threadSource":"feature:another_background_task"}})
        ));
    }

    #[test]
    fn overlapping_connections_can_reuse_rpc_ids_without_overwriting_selection() {
        let mut tracker = Tracker::default();
        tracker.request(1, &json!({"id":1,"method":"thread/start"}));
        tracker.request(2, &json!({"id":1,"method":"thread/resume"}));
        tracker.response(
            2,
            &json!({"id":1,"result":{"thread":{"id":"selected","name":"Selected"}}}),
        );
        tracker.response(
            1,
            &json!({"id":1,"result":{"thread":{"id":"previous","name":"Previous"}}}),
        );
        tracker.disconnected(2);
        assert_eq!(tracker.current.thread_id.as_deref(), Some("selected"));
        assert_eq!(tracker.current.label(), "Selected");
        assert!(tracker.selections.is_empty());
    }

    #[test]
    fn selection_is_correlated_and_other_threads_cannot_replace_it() {
        let mut t = Tracker::default();
        t.request(1, &json!({"id":1,"method":"thread/resume"}));
        t.response(
            1,
            &json!({"id":1,"result":{"thread":{"id":"a","name":"First","cwd":"/a"}}}),
        );
        assert_eq!(t.current.label(), "First");
        t.response(
            1,
            &json!({"method":"thread/started","params":{"thread":{"id":"other","name":"Wrong"}}}),
        );
        t.response(1, &json!({"method":"thread/name/updated","params":{"threadId":"other","threadName":"Wrong"}}));
        assert_eq!(t.current.thread_id.as_deref(), Some("a"));
        assert_eq!(t.current.label(), "First");
        t.request(
            1,
            &json!({"id":2,"method":"thread/read","params":{"threadId":"a"}}),
        );
        t.request(1, &json!({"id":3,"method":"thread/fork"}));
        t.response(
            1,
            &json!({"id":3,"result":{"thread":{"id":"b","name":"Second"}}}),
        );
        t.response(
            1,
            &json!({"id":2,"result":{"thread":{"id":"a","name":"Stale"}}}),
        );
        assert_eq!(t.current.label(), "Second");
        t.response(1, &json!({"method":"thread/name/updated","params":{"threadId":"b","threadName":"Renamed"}}));
        assert_eq!(t.current.label(), "Renamed");
    }

    #[test]
    fn failed_and_out_of_order_selections_do_not_corrupt_current_thread() {
        let mut t = Tracker::default();
        t.request(1, &json!({"id":1,"method":"thread/start"}));
        t.request(1, &json!({"id":2,"method":"thread/resume"}));
        t.response(
            1,
            &json!({"id":2,"result":{"thread":{"id":"new","preview":"Hello"}}}),
        );
        t.response(
            1,
            &json!({"id":1,"result":{"thread":{"id":"old","name":"Old"}}}),
        );
        assert_eq!(t.current.thread_id.as_deref(), Some("new"));
        assert_eq!(t.current.label(), "Hello");
        t.request(1, &json!({"id":3,"method":"thread/resume"}));
        t.response(1, &json!({"id":3,"error":{"message":"not found"}}));
        assert_eq!(t.current.thread_id.as_deref(), Some("new"));
    }
}
