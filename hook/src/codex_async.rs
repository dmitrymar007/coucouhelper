//! Async choice questions are typed agentMessage notifications, not server
//! requests. Reply using the same structured user input as the official UI.
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};

fn group(thread: &str, item: &str) -> String {
    format!("coucou-async:{}", json!([thread, item]))
}

pub fn request(msg: &Value) -> Option<Value> {
    if !matches!(msg["method"].as_str()?, "item/started" | "item/completed") {
        return None;
    }
    let params = msg.get("params")?;
    let item = params.get("item")?;
    if item["type"] != "agentMessage" {
        return None;
    }
    let thread = params.get("threadId")?.as_str()?;
    let id = item.get("id")?.as_str()?;
    let questions = item.get("questions")?.as_array()?;
    if questions.is_empty() || questions.len() > 4 {
        return None;
    }
    let mut titles = HashSet::new();
    let mut converted = Vec::new();
    for (index, q) in questions.iter().enumerate() {
        let title = q.get("title")?.as_str()?;
        let options = q.get("options")?.as_array()?;
        if title.is_empty() || !titles.insert(title) || options.is_empty() || options.len() > 6 {
            return None;
        }
        let options: Vec<Value> = options
            .iter()
            .map(|o| {
                let label = o.as_str()?;
                (!label.is_empty()).then(|| json!({"label":label,"description":""}))
            })
            .collect::<Option<_>>()?;
        converted.push(json!({
            "id":json!(["request_user_input_async",id,index]).to_string(),
            "question":title,"header":"Question","options":options
        }));
    }
    Some(
        json!({"id":group(thread,id),"method":"item/tool/requestUserInput","coucou_async":true,
        "params":{"threadId":thread,"questions":converted}}),
    )
}

/// Build only a user reply, never a policy or model override. App Server's
/// turn/start adds input to an active turn or starts a follow-up if it is idle.
pub fn submission(request: &Value, validated: &Value, rpc_id: &str) -> Option<Value> {
    let mut replies = Vec::new();
    for q in request["params"]["questions"].as_array()? {
        let id = q.get("id")?.as_str()?;
        let labels = validated["answers"][id]["answers"].as_array()?;
        if labels.len() != 1 {
            return None;
        }
        replies.push(
            json!({"questionItemId":id,"question":q["question"],"answer":labels[0].as_str()?}),
        );
    }
    let text = format!(
        "<send_user_message_question_reply>\n{}\n</send_user_message_question_reply>",
        json!(replies)
    );
    Some(json!({"id":rpc_id,"method":"turn/start","params":{
        "threadId":request["params"]["threadId"],"input":[{"type":"text","text":text,"text_elements":[]}]
    }}))
}

/// Identify native replies without interpreting any other user messages.
pub fn reply_groups(msg: &Value) -> Option<HashSet<String>> {
    if !matches!(msg["method"].as_str()?, "turn/start" | "turn/steer") {
        return None;
    }
    let params = msg.get("params")?;
    let thread = params.get("threadId")?.as_str()?;
    let input = params.get("input")?.as_array()?;
    if input.len() != 1 || input[0]["type"] != "text" {
        return None;
    }
    let text = input[0].get("text")?.as_str()?.trim();
    let text = text
        .strip_prefix("<send_user_message_question_reply>")?
        .strip_suffix("</send_user_message_question_reply>")?;
    let raw: Value = serde_json::from_str(text).ok()?;
    let replies = match raw {
        Value::Array(a) => a,
        Value::Object(_) => vec![raw],
        _ => return None,
    };
    if replies.is_empty() {
        return None;
    }
    let mut groups = HashSet::new();
    for reply in replies {
        reply.get("question")?.as_str()?;
        reply.get("answer")?.as_str()?;
        let id: Value = serde_json::from_str(reply.get("questionItemId")?.as_str()?).ok()?;
        let parts = id.as_array()?;
        if parts.len() != 3 || parts[0] != "request_user_input_async" || !parts[2].is_u64() {
            return None;
        }
        groups.insert(group(thread, parts[1].as_str()?));
    }
    Some(groups)
}

pub fn notification_reply_groups(msg: &Value) -> Option<HashSet<String>> {
    if !matches!(msg["method"].as_str()?, "item/started" | "item/completed") {
        return None;
    }
    let params = msg.get("params")?;
    let item = params.get("item")?;
    if item["type"] != "userMessage" {
        return None;
    }
    reply_groups(
        &json!({"method":"turn/start","params":{"threadId":params["threadId"],"input":item["content"]}}),
    )
}

struct Flight {
    group: String,
    waiting: Vec<Value>,
}
#[derive(Default)]
pub struct Replies {
    flights: HashMap<String, Flight>,
    cached: HashMap<String, Value>,
    order: VecDeque<String>,
}
impl Replies {
    pub fn sent(&mut self, group: String, id: &str) {
        self.flights.insert(
            json!(id).to_string(),
            Flight {
                group,
                waiting: Vec::new(),
            },
        );
    }
    /// The response to a bridge-owned RPC must not reach a client that never
    /// sent it. Competing native submissions receive that RPC's real outcome.
    pub fn response(&mut self, msg: &Value) -> Option<Vec<Value>> {
        if msg.get("method").is_some()
            || !(msg.get("result").is_some() || msg.get("error").is_some())
        {
            return None;
        }
        let flight = self.flights.remove(&msg.get("id")?.to_string())?;
        let replies = flight
            .waiting
            .iter()
            .map(|client| acknowledge(client, msg))
            .collect();
        if msg.get("result").is_some() {
            self.cached.insert(flight.group.clone(), msg.clone());
            self.order.push_back(flight.group);
            if self.order.len() > 256 {
                if let Some(old) = self.order.pop_front() {
                    self.cached.remove(&old);
                }
            }
        }
        Some(replies)
    }
    /// Some(None): defer the competing native request until the bridge's
    /// submission is acknowledged. None: pass the native input through.
    pub fn duplicate(&mut self, msg: &Value, groups: &HashSet<String>) -> Option<Option<Value>> {
        if groups.len() != 1 || msg.get("id").is_none() {
            return None;
        }
        let group = groups.iter().next()?;
        if let Some(reply) = self.cached.get(group) {
            return Some(Some(acknowledge(msg, reply)));
        }
        if let Some(flight) = self.flights.values_mut().find(|f| &f.group == group) {
            if flight.waiting.len() < 32 {
                flight.waiting.push(msg.clone());
                return Some(None);
            }
        }
        None
    }
}
fn acknowledge(client: &Value, upstream: &Value) -> Value {
    if let Some(err) = upstream.get("error") {
        return json!({"id":client["id"],"error":err});
    }
    if client["method"] == "turn/steer" {
        if let Some(id) = upstream.pointer("/result/turn/id").and_then(Value::as_str) {
            return json!({"id":client["id"],"result":{"turnId":id}});
        }
    }
    json!({"id":client["id"],"result":upstream["result"]})
}

#[cfg(test)]
mod tests {
    use super::*;
    pub fn notification() -> Value {
        json!({"method":"item/completed","params":{"threadId":"t","item":{
            "type":"agentMessage","id":"call_123","delivery":"async","text":"", "questions":[{"title":"Choose?","options":["A","B"]}]
        }}})
    }
    #[test]
    fn actual_async_message_shape_round_trips_through_a_user_reply() {
        let request = request(&notification()).unwrap();
        assert_eq!(
            request["params"]["questions"][0]["id"],
            r#"["request_user_input_async","call_123",0]"#
        );
        let chosen =
            json!({"answers":{r#"["request_user_input_async","call_123",0]"#:{"answers":["B"]}}});
        let sent = submission(&request, &chosen, "bridge-rpc").unwrap();
        assert_eq!(sent["method"], "turn/start");
        assert!(reply_groups(&sent)
            .unwrap()
            .contains(request["id"].as_str().unwrap()));
        assert!(sent["params"].get("approvalPolicy").is_none());
        assert!(sent["params"].get("approvalsReviewer").is_none());
    }
    #[test]
    fn competing_native_submission_is_acked_but_not_sent_twice() {
        let request = request(&notification()).unwrap();
        let chosen =
            json!({"answers":{r#"["request_user_input_async","call_123",0]"#:{"answers":["B"]}}});
        let client = submission(&request, &chosen, "native-id").unwrap();
        let groups = reply_groups(&client).unwrap();
        let mut replies = Replies::default();
        replies.sent(request["id"].as_str().unwrap().into(), "bridge-rpc");
        assert_eq!(replies.duplicate(&client, &groups), Some(None));
        let upstream = json!({"id":"bridge-rpc","result":{"turn":{"id":"turn1","status":"inProgress","items":[]}}});
        let ack = replies.response(&upstream).unwrap();
        assert_eq!(ack[0]["id"], "native-id");
        assert_eq!(ack[0]["result"], upstream["result"]);
        assert_eq!(
            replies.duplicate(&client, &groups).unwrap().unwrap()["id"],
            "native-id"
        );
    }
    #[test]
    fn free_text_only_and_ordinary_messages_are_not_interpreted_as_choices() {
        let mut msg = notification();
        msg["params"]["item"]["questions"][0]["options"] = Value::Null;
        assert!(request(&msg).is_none());
        assert!(reply_groups(&json!({"method":"turn/start","params":{"threadId":"t","input":[{"type":"text","text":"hello"}]}})).is_none());
    }
}
