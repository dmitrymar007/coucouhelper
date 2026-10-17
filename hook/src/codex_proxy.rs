//! Transparent stdio bridge: native VS Code prompts and island cards race for
//! the same App Server request. Only the first explicit answer is forwarded.
//! No transcript, authentication message or tool output is saved or logged.
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, BufRead, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{mpsc, Arc, Mutex};

const TRANSPORT: &str = "codex-app-server";
static CHILD_PID: AtomicI32 = AtomicI32::new(0);

extern "C" fn forward_signal(signal: libc::c_int) {
    let pid = CHILD_PID.load(Ordering::Relaxed);
    if pid > 0 {
        unsafe {
            libc::kill(pid, signal);
        }
    }
    unsafe {
        libc::_exit(128 + signal);
    }
}

enum Event {
    Client(Vec<u8>),
    Server(Vec<u8>),
    ClientEnd,
    ServerEnd,
    Answer(String, u64, String),
}
#[derive(Default)]
struct Cancellation {
    cancelled: AtomicBool,
    stream: Mutex<Option<UnixStream>>,
}
impl Cancellation {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Some(stream) = self.stream.lock().unwrap().take() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}
struct Pending {
    generation: u64,
    request: Value,
    cancel: Arc<Cancellation>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Run all non-app-server invocations directly; VS Code also asks --version.
pub fn run(args: Vec<std::ffi::OsString>) -> i32 {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    let Some(real) = crate::codex_binary::find(&home) else {
        eprintln!("Coucou: the official Codex VS Code extension binary was not found.");
        return 1;
    };
    let mut command = Command::new(real);
    command.args(&args);
    if !args.iter().any(|arg| arg == "app-server") {
        use std::os::unix::process::CommandExt;
        eprintln!("Coucou: cannot run Codex: {}", command.exec());
        return 1;
    }
    // Only Coucou's PermissionRequest hook yields to the native prompt. Other
    // hooks, sandbox policies and approval reviewers remain in force.
    command.env("COUCOU_CODEX_BRIDGE", "1");
    let mut child = match command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            eprintln!("Coucou: cannot start Codex: {err}");
            return 1;
        }
    };
    CHILD_PID.store(child.id() as i32, Ordering::Relaxed);
    unsafe {
        for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            libc::signal(signal, forward_signal as *const () as libc::sighandler_t);
        }
    }
    let input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::sync_channel(256);
    let client_tx = tx.clone();
    std::thread::spawn(move || read_lines(io::stdin().lock(), client_tx, true));
    let server_tx = tx.clone();
    std::thread::spawn(move || read_lines(io::BufReader::new(output), server_tx, false));
    let result = pump(input, io::stdout().lock(), rx, tx, dispatch_island);
    if result.is_err() {
        let _ = child.kill();
    }
    let code = child.wait().ok().and_then(|s| s.code()).unwrap_or(1);
    CHILD_PID.store(0, Ordering::Relaxed);
    code
}

fn read_lines(mut reader: impl BufRead, tx: mpsc::SyncSender<Event>, client: bool) {
    loop {
        let mut line = Vec::new();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                if tx
                    .send(if client {
                        Event::Client(line)
                    } else {
                        Event::Server(line)
                    })
                    .is_err()
                {
                    return;
                }
            }
        }
    }
    let _ = tx.send(if client {
        Event::ClientEnd
    } else {
        Event::ServerEnd
    });
}

fn write_line(writer: &mut impl Write, line: &[u8]) -> io::Result<()> {
    writer.write_all(line)?;
    writer.flush()
}

fn pump(
    mut child: impl Write,
    mut parent: impl Write,
    rx: mpsc::Receiver<Event>,
    tx: mpsc::SyncSender<Event>,
    dispatch: impl Fn(Value, Arc<Cancellation>, mpsc::SyncSender<Event>, String, u64),
) -> io::Result<()> {
    let mut pending: HashMap<String, Pending> = HashMap::new();
    let mut answered = HashSet::new();
    let mut order = VecDeque::new();
    let mut generation = 0;
    let mut cwds: HashMap<String, String> = HashMap::new();
    let mut file_changes: HashMap<String, Value> = HashMap::new();
    let mut async_seen = HashSet::new();
    let mut async_order = VecDeque::new();
    let mut async_replies = crate::codex_async::Replies::default();
    while let Ok(event) = rx.recv() {
        match event {
            Event::Server(line) => {
                let parsed = serde_json::from_slice::<Value>(&line).ok();
                if let Some(msg) = &parsed {
                    if let Some(acks) = async_replies.response(msg) {
                        for ack in acks {
                            write_line(&mut parent, format!("{ack}\n").as_bytes())?;
                        }
                        continue;
                    }
                }
                // Native messages arrive before any island connection attempt.
                write_line(&mut parent, &line)?;
                let Some(msg) = parsed else { continue };
                if let Some(groups) = crate::codex_async::notification_reply_groups(&msg) {
                    for group in groups {
                        pending.remove(&group);
                    }
                }
                if matches!(
                    msg["method"].as_str(),
                    Some("thread/closed" | "thread/archived" | "thread/deleted")
                ) {
                    if let Some(thread) = msg["params"]["threadId"].as_str() {
                        pending.retain(|_, p| {
                            p.request["params"]["threadId"].as_str() != Some(thread)
                        });
                        cwds.remove(thread);
                    }
                }
                if let Some(thread) = msg
                    .pointer("/params/thread")
                    .or_else(|| msg.pointer("/result/thread"))
                {
                    if let (Some(id), Some(cwd)) = (thread["id"].as_str(), thread["cwd"].as_str()) {
                        if cwds.len() >= 256 {
                            cwds.clear();
                        }
                        cwds.insert(id.into(), cwd.into());
                    }
                }
                if msg["method"] == "item/started" && msg["params"]["item"]["type"] == "fileChange"
                {
                    if let (Some(id), Some(changes)) = (
                        msg["params"]["item"]["id"].as_str(),
                        msg["params"]["item"].get("changes"),
                    ) {
                        if file_changes.len() >= 32 {
                            file_changes.clear();
                        }
                        file_changes.insert(id.into(), changes.clone());
                    }
                }
                if msg["method"] == "item/completed" {
                    if let Some(id) = msg["params"]["item"]["id"].as_str() {
                        file_changes.remove(id);
                    }
                }
                if msg["method"] == "serverRequest/resolved" {
                    pending.remove(&msg["params"]["requestId"].to_string());
                    continue;
                }
                let request = if let Some(request) = crate::codex_async::request(&msg) {
                    let key = request["id"].as_str().unwrap().to_string();
                    if !async_seen.insert(key.clone()) {
                        continue;
                    }
                    async_order.push_back(key);
                    if async_order.len() > 256 {
                        if let Some(old) = async_order.pop_front() {
                            async_seen.remove(&old);
                        }
                    }
                    request
                } else {
                    if msg.get("method").is_none()
                        || !msg
                            .get("id")
                            .is_some_and(|id| id.is_string() || id.is_number())
                    {
                        continue;
                    }
                    msg
                };
                let key = if request["coucou_async"] == true {
                    request["id"].as_str().unwrap().to_string()
                } else {
                    request["id"].to_string()
                };
                answered.remove(&key);
                let msg = request;
                let cwd = msg
                    .pointer("/params/cwd")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        msg.pointer("/params/threadId")
                            .and_then(Value::as_str)
                            .and_then(|id| cwds.get(id).map(String::as_str))
                    })
                    .unwrap_or("");
                let changes = msg
                    .pointer("/params/itemId")
                    .and_then(Value::as_str)
                    .and_then(|id| file_changes.get(id));
                let Some(payload) = island_request(&msg, cwd, changes) else {
                    continue;
                };
                // A broken host should not create unbounded island workers.
                if pending.len() >= 64 {
                    continue;
                }
                generation += 1;
                let cancel = Arc::new(Cancellation::default());
                pending.insert(
                    key.clone(),
                    Pending {
                        generation,
                        request: msg,
                        cancel: cancel.clone(),
                    },
                );
                dispatch(payload, cancel, tx.clone(), key, generation);
            }
            Event::Client(line) => {
                if let Ok(msg) = serde_json::from_slice::<Value>(&line) {
                    if let Some(groups) = crate::codex_async::reply_groups(&msg) {
                        for group in &groups {
                            pending.remove(group);
                        }
                        if let Some(ack) = async_replies.duplicate(&msg, &groups) {
                            if let Some(ack) = ack {
                                write_line(&mut parent, format!("{ack}\n").as_bytes())?;
                            }
                            continue;
                        }
                    }
                    if msg.get("method").is_none()
                        && (msg.get("result").is_some() || msg.get("error").is_some())
                    {
                        let key = msg["id"].to_string();
                        // Late native answer after the island answered: swallow
                        // the duplicate, leaving the first decision intact.
                        if answered.contains(&key) {
                            continue;
                        }
                        pending.remove(&key);
                    }
                }
                write_line(&mut child, &line)?;
            }
            Event::Answer(key, generation, answer) => {
                let Some(p) = pending.get(&key).filter(|p| p.generation == generation) else {
                    continue;
                };
                let Some(result) = island_answer(&p.request, &answer) else {
                    continue;
                };
                if p.request["coucou_async"] == true {
                    let id = format!("coucou-async-reply-{}-{generation}", std::process::id());
                    let Some(submission) = crate::codex_async::submission(&p.request, &result, &id)
                    else {
                        continue;
                    };
                    write_line(&mut child, format!("{submission}\n").as_bytes())?;
                    async_replies.sent(key.clone(), &id);
                } else {
                    let response = json!({"id":p.request["id"], "result":result});
                    write_line(&mut child, format!("{response}\n").as_bytes())?;
                }
                pending.remove(&key);
                answered.insert(key.clone());
                order.push_back(key);
                if order.len() > 256 {
                    if let Some(old) = order.pop_front() {
                        answered.remove(&old);
                    }
                }
                // App Server emits serverRequest/resolved for the native UI.
            }
            Event::ClientEnd | Event::ServerEnd => break,
        }
    }
    Ok(())
}

fn dispatch_island(
    payload: Value,
    cancel: Arc<Cancellation>,
    tx: mpsc::SyncSender<Event>,
    key: String,
    generation: u64,
) {
    std::thread::spawn(move || {
        if let Some(answer) = ask_island(payload, &cancel) {
            let _ = tx.send(Event::Answer(key, generation, answer));
        }
    });
}

fn ask_island(payload: Value, cancel: &Cancellation) -> Option<String> {
    if cancel.cancelled.load(Ordering::SeqCst) {
        return None;
    }
    let line = format!("{payload}\n");
    if line.len() > 1 << 20 {
        return None;
    }
    ask_stream(line.as_bytes(), cancel, crate::unix::connect()?)
}

fn ask_stream(line: &[u8], cancel: &Cancellation, mut stream: UnixStream) -> Option<String> {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(110)))
        .ok()?;
    stream
        .set_write_timeout(Some(std::time::Duration::from_millis(300)))
        .ok()?;
    {
        let mut socket = cancel.stream.lock().ok()?;
        if cancel.cancelled.load(Ordering::SeqCst) {
            return None;
        }
        *socket = Some(stream.try_clone().ok()?);
    }
    stream.write_all(line).ok()?;
    let mut answer = Vec::new();
    let mut byte = [0u8; 1];
    while answer.len() < 64 * 1024 {
        if stream.read(&mut byte).ok()? == 0 {
            break;
        }
        if byte[0] == b'\n' {
            break;
        }
        answer.push(byte[0]);
    }
    if answer.is_empty() || cancel.cancelled.load(Ordering::SeqCst) {
        return None;
    }
    String::from_utf8(answer).ok()
}

fn island_request(msg: &Value, cwd: &str, changes: Option<&Value>) -> Option<Value> {
    let params = msg.get("params")?;
    let thread = params.get("threadId")?.as_str()?;
    let method = msg.get("method")?.as_str()?;
    let (tool, input) = match method {
        "item/commandExecution/requestApproval" => {
            if params
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("command")
                != "command"
            {
                return None;
            }
            if let Some(choices) = params.get("availableDecisions").filter(|v| !v.is_null()) {
                let choices = choices.as_array()?;
                if !["accept", "decline"]
                    .iter()
                    .all(|choice| choices.iter().any(|v| v.as_str() == Some(choice)))
                {
                    return None;
                }
            }
            let command = if let Some(network) = params
                .get("networkApprovalContext")
                .filter(|v| v.is_object())
            {
                format!(
                    "Network access: {} ({})",
                    network["host"].as_str()?,
                    network["protocol"].as_str()?
                )
            } else {
                params.get("command")?.as_str()?.to_string()
            };
            (
                "Bash",
                json!({"command":command, "description":params["reason"]}),
            )
        }
        "item/fileChange/requestApproval" => {
            // A reason supplied by the model is not a replacement for the diff.
            // If this server did not send its fileChange item, keep approval in VS Code.
            let changes = changes?.as_array()?;
            if changes.is_empty() {
                return None;
            }
            let mut command = serde_json::to_string_pretty(changes).ok()?;
            if let Some(root) = params.get("grantRoot").and_then(Value::as_str) {
                command = format!("Requested write root: {root}\n{command}");
            }
            (
                "Edit",
                json!({"command":command,"description":params["reason"]}),
            )
        }
        "item/permissions/requestApproval" => (
            "Permissions",
            json!({
                "command":serde_json::to_string_pretty(params.get("permissions")?).ok()?, "description":params["reason"]
            }),
        ),
        "item/tool/requestUserInput" => {
            let questions = params.get("questions")?.as_array()?;
            if questions.is_empty() || questions.len() > 4 {
                return None;
            }
            let mut texts = HashSet::new();
            let mut ids = HashSet::new();
            for q in questions {
                let options = q.get("options")?.as_array()?;
                if q["isSecret"] == true
                    || options.is_empty()
                    || options.len() > 6
                    || !texts.insert(q.get("question")?.as_str()?)
                    || !ids.insert(q.get("id")?.as_str()?)
                    || options.iter().any(|o| {
                        o.get("label")
                            .and_then(Value::as_str)
                            .is_none_or(str::is_empty)
                    })
                {
                    return None;
                }
            }
            ("AskUserQuestion", json!({"questions":questions}))
        }
        _ => return None,
    };
    Some(json!({
        "hook_event_name":"PermissionRequest", "coucou_agent":"codex", "coucou_transport":TRANSPORT,
        "session_id":thread, "cwd":cwd, "tool_name":tool, "tool_input":input,
        "coucou_async":msg["coucou_async"] == true,
        "ancestor_pids":crate::unix::ancestor_pids()
    }))
}

fn island_answer(msg: &Value, answer: &str) -> Option<Value> {
    match msg["method"].as_str()? {
        "item/tool/requestUserInput" => {
            let raw: Value = serde_json::from_str(answer).ok()?;
            let answers = raw.get("answers")?.as_object()?;
            let mut result = serde_json::Map::new();
            for q in msg["params"]["questions"].as_array()? {
                let value = answers.get(q["question"].as_str()?)?;
                let labels = if let Some(label) = value.as_str() {
                    vec![label]
                } else {
                    value
                        .as_array()?
                        .iter()
                        .map(Value::as_str)
                        .collect::<Option<Vec<_>>>()?
                };
                if labels.is_empty()
                    || labels.iter().any(|label| {
                        !q["options"].as_array().is_some_and(|options| {
                            options.iter().any(|o| o["label"].as_str() == Some(label))
                        })
                    })
                {
                    return None;
                }
                result.insert(q["id"].as_str()?.into(), json!({"answers":labels}));
            }
            Some(json!({"answers":result}))
        }
        "item/permissions/requestApproval" => match answer.trim() {
            "allow" => Some(json!({"permissions":msg["params"]["permissions"], "scope":"turn"})),
            "deny" => Some(json!({"permissions":{}, "scope":"turn"})),
            _ => None,
        },
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            match answer.trim() {
                "allow" => Some(json!({"decision":"accept"})),
                "deny" => Some(json!({"decision":"decline"})),
                _ => None,
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn question() -> Value {
        json!({"id":11,"method":"item/tool/requestUserInput","params":{
            "threadId":"t", "questions":[{"id":"choice","question":"Which one?","header":"Choice", "options":[{"label":"A"},{"label":"B"}]}]
        }})
    }
    #[test]
    fn questions_round_trip_by_id_without_guessing_answers() {
        let q = question();
        assert_eq!(
            island_request(&q, "/project", None).unwrap()["tool_name"],
            "AskUserQuestion"
        );
        assert_eq!(
            island_answer(&q, r#"{"answers":{"Which one?":"B"}}"#).unwrap(),
            json!({"answers":{"choice":{"answers":["B"]}}})
        );
        assert!(island_answer(&q, r#"{"answers":{"Which one?":"injected"}}"#).is_none());
        assert!(island_answer(&q, "allow").is_none());
        let mut secret = q;
        secret["params"]["questions"][0]["isSecret"] = json!(true);
        assert!(island_request(&secret, "", None).is_none());
    }
    #[test]
    fn approvals_never_add_session_or_policy_grants() {
        for method in [
            "item/commandExecution/requestApproval",
            "item/fileChange/requestApproval",
        ] {
            let msg = json!({"method":method});
            assert_eq!(
                island_answer(&msg, "allow").unwrap(),
                json!({"decision":"accept"})
            );
            assert_eq!(
                island_answer(&msg, "deny").unwrap(),
                json!({"decision":"decline"})
            );
            assert!(island_answer(&msg, "always").is_none());
        }
        assert_eq!(island_answer(&json!({"method":"item/permissions/requestApproval","params":{"permissions":{"network":{"enabled":true}}}}), "deny").unwrap()["permissions"], json!({}));
    }
    #[test]
    fn first_answer_wins_and_native_messages_pass_through_unchanged() {
        let request = json!({"id":"req-1","method":"item/commandExecution/requestApproval","params":{
            "threadId":"t", "command":"echo hello"
        }});
        let raw = format!("{request}\n").into_bytes();
        let native = b"{ \"id\": \"req-1\", \"result\": {\"decision\":\"decline\"} }\n".to_vec();
        for island_first in [true, false] {
            let (tx, rx) = mpsc::sync_channel(256);
            tx.send(Event::Server(raw.clone())).unwrap();
            if island_first {
                tx.send(Event::Answer("\"req-1\"".into(), 1, "allow".into()))
                    .unwrap();
            }
            tx.send(Event::Client(native.clone())).unwrap();
            if !island_first {
                tx.send(Event::Answer("\"req-1\"".into(), 1, "allow".into()))
                    .unwrap();
            }
            tx.send(Event::ServerEnd).unwrap();
            let (mut child, mut parent) = (Vec::new(), Vec::new());
            pump(&mut child, &mut parent, rx, tx, |_, _, _, _, _| {}).unwrap();
            assert_eq!(parent, raw, "VS Code receives its prompt verbatim");
            let lines: Vec<Value> = String::from_utf8(child.clone())
                .unwrap()
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect();
            assert_eq!(
                lines.len(),
                1,
                "a competing answer must never be sent twice"
            );
            if island_first {
                assert_eq!(lines[0]["result"]["decision"], "accept");
            } else {
                assert_eq!(child, native, "native answer is forwarded byte for byte");
            }
        }
    }

    #[test]
    fn resolved_or_cancelled_request_cannot_receive_an_island_answer() {
        let (tx, rx) = mpsc::sync_channel(256);
        tx.send(Event::Server(format!("{}\n", question()).into_bytes()))
            .unwrap();
        let resolved = b"{\"method\":\"serverRequest/resolved\",\"params\":{\"threadId\":\"t\",\"requestId\":11}}\n";
        tx.send(Event::Server(resolved.to_vec())).unwrap();
        tx.send(Event::Answer(
            "11".into(),
            1,
            r#"{"answers":{"Which one?":"A"}}"#.into(),
        ))
        .unwrap();
        tx.send(Event::ServerEnd).unwrap();
        let (mut child, mut parent) = (Vec::new(), Vec::new());
        pump(&mut child, &mut parent, rx, tx, |_, _, _, _, _| {}).unwrap();
        assert!(child.is_empty());
        assert!(parent.ends_with(resolved));
    }
    #[test]
    fn native_response_cancels_the_socket_wait_immediately() {
        let (client, server) = UnixStream::pair().unwrap();
        let cancel = Arc::new(Cancellation::default());
        let worker_cancel = cancel.clone();
        let worker =
            std::thread::spawn(move || ask_stream(b"{\"test\":true}\n", &worker_cancel, client));
        let mut reader = io::BufReader::new(server);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line, "{\"test\":true}\n");
        cancel.cancel();
        assert!(worker.join().unwrap().is_none());
        let mut rest = String::new();
        reader.read_to_string(&mut rest).unwrap();
        assert!(
            rest.is_empty(),
            "closing the socket tells Coucou the native UI answered"
        );
    }

    #[test]
    fn island_socket_answer_is_read_without_touching_the_native_transport() {
        let (client, server) = UnixStream::pair().unwrap();
        let worker =
            std::thread::spawn(move || ask_stream(b"{}\n", &Cancellation::default(), client));
        let mut reader = io::BufReader::new(server);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        reader.get_mut().write_all(b"deny\n").unwrap();
        assert_eq!(worker.join().unwrap().as_deref(), Some("deny"));
    }

    #[test]
    fn file_approval_requires_the_real_changes_and_restricted_decisions_stay_native() {
        let msg =
            json!({"id":1,"method":"item/fileChange/requestApproval","params":{"threadId":"t"}});
        assert!(island_request(&msg, "", None).is_none());
        let changes =
            json!([{"path":"/project/file.txt","kind":{"type":"update"},"diff":"-old\n+new"}]);
        let shown = island_request(&msg, "", Some(&changes)).unwrap();
        assert!(shown["tool_input"]["command"]
            .as_str()
            .unwrap()
            .contains("/project/file.txt"));
        let msg = json!({"id":1,"method":"item/commandExecution/requestApproval","params":{
            "threadId":"t","command":"echo hello","availableDecisions":["acceptForSession","decline"]
        }});
        assert!(island_request(&msg, "", None).is_none());
    }
    fn async_question() -> Value {
        json!({"method":"item/completed","params":{"threadId":"t","item":{
            "type":"agentMessage","id":"call_async","delivery":"async","text":"", "questions":[{"title":"Choose?","options":["A","B"]}]
        }}})
    }
    #[test]
    fn async_island_answer_becomes_user_input_and_never_a_server_response() {
        let notification = async_question();
        let request = crate::codex_async::request(&notification).unwrap();
        let key = request["id"].as_str().unwrap().to_string();
        let (tx, rx) = mpsc::sync_channel(256);
        tx.send(Event::Server(format!("{notification}\n").into_bytes()))
            .unwrap();
        tx.send(Event::Answer(
            key,
            1,
            r#"{"answers":{"Choose?":"B"}}"#.into(),
        ))
        .unwrap();
        tx.send(Event::ServerEnd).unwrap();
        let (mut child, mut parent) = (Vec::new(), Vec::new());
        pump(&mut child, &mut parent, rx, tx, |payload, _, _, _, _| {
            assert_eq!(payload["coucou_async"], true);
            assert_eq!(payload["tool_input"]["questions"][0]["question"], "Choose?");
        })
        .unwrap();
        let submitted: Value = serde_json::from_slice(&child).unwrap();
        assert_eq!(submitted["method"], "turn/start");
        assert!(crate::codex_async::reply_groups(&submitted).is_some());
        assert_eq!(parent, format!("{notification}\n").as_bytes());
    }
    #[test]
    fn native_async_answer_cancels_the_island_and_remains_verbatim() {
        let notification = async_question();
        let request = crate::codex_async::request(&notification).unwrap();
        let key = request["id"].as_str().unwrap().to_string();
        let validated = island_answer(&request, r#"{"answers":{"Choose?":"A"}}"#).unwrap();
        let native = crate::codex_async::submission(&request, &validated, "native").unwrap();
        let native_bytes = format!("{native}\n").into_bytes();
        let (tx, rx) = mpsc::sync_channel(256);
        tx.send(Event::Server(format!("{notification}\n").into_bytes()))
            .unwrap();
        tx.send(Event::Client(native_bytes.clone())).unwrap();
        tx.send(Event::Answer(
            key,
            1,
            r#"{"answers":{"Choose?":"B"}}"#.into(),
        ))
        .unwrap();
        tx.send(Event::ServerEnd).unwrap();
        let (mut child, mut parent) = (Vec::new(), Vec::new());
        pump(&mut child, &mut parent, rx, tx, |_, _, _, _, _| {}).unwrap();
        assert_eq!(child, native_bytes);
    }
    #[test]
    fn async_reply_from_another_client_also_closes_the_island_card() {
        let notification = async_question();
        let request = crate::codex_async::request(&notification).unwrap();
        let key = request["id"].as_str().unwrap().to_string();
        let validated = island_answer(&request, r#"{"answers":{"Choose?":"A"}}"#).unwrap();
        let native = crate::codex_async::submission(&request, &validated, "native").unwrap();
        let received = json!({"method":"item/started","params":{"threadId":"t","item":{"type":"userMessage","content":native["params"]["input"]}}});
        let (tx, rx) = mpsc::sync_channel(256);
        tx.send(Event::Server(format!("{notification}\n").into_bytes()))
            .unwrap();
        tx.send(Event::Server(format!("{received}\n").into_bytes()))
            .unwrap();
        tx.send(Event::Answer(
            key,
            1,
            r#"{"answers":{"Choose?":"B"}}"#.into(),
        ))
        .unwrap();
        tx.send(Event::ServerEnd).unwrap();
        let (mut child, mut parent) = (Vec::new(), Vec::new());
        pump(&mut child, &mut parent, rx, tx, |_, _, _, _, _| {}).unwrap();
        assert!(child.is_empty());
    }
    #[test]
    fn async_rpc_ack_is_consumed_and_late_native_submit_gets_the_real_ack() {
        let notification = async_question();
        let request = crate::codex_async::request(&notification).unwrap();
        let key = request["id"].as_str().unwrap().to_string();
        let validated = island_answer(&request, r#"{"answers":{"Choose?":"A"}}"#).unwrap();
        let native = crate::codex_async::submission(&request, &validated, "native").unwrap();
        let ack = json!({"id":format!("coucou-async-reply-{}-1",std::process::id()),"result":{"turn":{"id":"new-turn","status":"inProgress","items":[]}}});
        let (tx, rx) = mpsc::sync_channel(256);
        tx.send(Event::Server(format!("{notification}\n").into_bytes()))
            .unwrap();
        tx.send(Event::Answer(
            key,
            1,
            r#"{"answers":{"Choose?":"A"}}"#.into(),
        ))
        .unwrap();
        tx.send(Event::Server(format!("{ack}\n").into_bytes()))
            .unwrap();
        tx.send(Event::Client(format!("{native}\n").into_bytes()))
            .unwrap();
        tx.send(Event::ServerEnd).unwrap();
        let (mut child, mut parent) = (Vec::new(), Vec::new());
        pump(&mut child, &mut parent, rx, tx, |_, _, _, _, _| {}).unwrap();
        assert_eq!(String::from_utf8(child).unwrap().lines().count(), 1);
        let received: Vec<Value> = String::from_utf8(parent)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(received.len(), 2);
        assert_eq!(received[1]["id"], "native");
        assert_eq!(received[1]["result"], ack["result"]);
    }
}
