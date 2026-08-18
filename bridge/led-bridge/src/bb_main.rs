//! Linux/BlueZ bridge from bb's realtime thread state to the Go60 LED GATT
//! service. The existing `paseo-led-bridge` binary remains available for the
//! old Windows setup; this binary is intentionally bb + Omarchy focused.

#[cfg(target_os = "linux")]
mod linux {
    use anyhow::{anyhow, bail, Context, Result};
    use bluer::gatt::remote::Characteristic;
    use futures_util::{SinkExt, StreamExt};
    use serde::Deserialize;
    use serde_json::json;
    use std::cmp::Ordering;
    use std::collections::HashMap;
    use std::env;
    use std::time::Duration;
    use tokio::time::{interval, sleep, MissedTickBehavior};
    use tokio_tungstenite::tungstenite::Message;
    use uuid::Uuid;

    const SERVICE_UUID: Uuid = Uuid::from_u128(0x70617365_6f4c_4544_b0a0_000000000001);
    const WRITE_UUID: Uuid = Uuid::from_u128(0x70617365_6f4c_4544_b0a0_000000000002);

    const OFF: Rgb = Rgb(0, 0, 0);
    const IDLE: Rgb = Rgb(10, 10, 10);
    const WORKING: Rgb = Rgb(0, 51, 255);
    const QUESTION: Rgb = Rgb(255, 95, 0);
    const ATTENTION: Rgb = Rgb(0, 200, 0);
    const ERROR: Rgb = Rgb(255, 0, 0);
    const FOCUS_BEACON: Rgb = Rgb(32, 32, 32);

    const IDX_Y: u8 = 10;
    const IDX_U: u8 = 11;
    const IDX_F: u8 = 16;
    const FILL_ALL: u8 = 0xfe;

    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    struct Rgb(u8, u8, u8);

    #[derive(Clone, Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ThreadEntry {
        id: String,
        status: String,
        project_id: String,
        archived_at: Option<u64>,
        pinned_at: Option<u64>,
        updated_at: u64,
        environment_id: Option<String>,
        last_read_at: Option<u64>,
        latest_attention_at: u64,
        #[serde(default)]
        has_pending_interaction: bool,
        #[serde(default)]
        activity: Activity,
        runtime: Runtime,
    }

    #[derive(Clone, Debug, Default, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Activity {
        #[serde(default)]
        active_workflow_count: u64,
        #[serde(default)]
        active_background_agent_count: u64,
        #[serde(default)]
        active_background_command_count: u64,
        #[serde(default)]
        active_plan_mode_count: u64,
        #[serde(default)]
        active_goal_count: u64,
    }

    #[derive(Clone, Debug, Default, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Runtime {
        #[serde(default)]
        display_status: String,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct SidebarBootstrap {
        projects: Vec<SidebarProject>,
        personal_project: SidebarProject,
    }

    #[derive(Debug, Deserialize)]
    struct SidebarProject {
        threads: Vec<ThreadEntry>,
    }

    #[derive(Debug, Deserialize)]
    struct LaterRpcResponse {
        result: LaterResult,
    }

    #[derive(Debug, Deserialize)]
    struct LaterResult {
        rows: Vec<LaterRow>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct LaterRow {
        thread_id: String,
        placed_at: u64,
    }

    #[derive(Debug, Deserialize)]
    struct ThreadOrderRpcResponse {
        result: ThreadOrderResult,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ThreadOrderResult {
        thread_ids: Vec<String>,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum SidebarBucket {
        Pinned,
        Unread,
        Active,
        NeedsInput,
        Idle,
        Later,
        Archived,
    }

    const SIDEBAR_BUCKETS: [SidebarBucket; 7] = [
        SidebarBucket::Pinned,
        SidebarBucket::Unread,
        SidebarBucket::Active,
        SidebarBucket::NeedsInput,
        SidebarBucket::Idle,
        SidebarBucket::Later,
        SidebarBucket::Archived,
    ];

    impl ThreadEntry {
        fn is_unread(&self) -> bool {
            self.last_read_at.unwrap_or_default() < self.latest_attention_at
        }

        fn is_active(&self) -> bool {
            let activity = &self.activity;
            activity.active_workflow_count
                + activity.active_background_agent_count
                + activity.active_background_command_count
                + activity.active_plan_mode_count
                + activity.active_goal_count
                > 0
                || matches!(
                    self.runtime.display_status.as_str(),
                    "active" | "host-reconnecting" | "provisioning" | "starting" | "stopping"
                )
        }

        fn needs_input(&self) -> bool {
            self.has_pending_interaction || (self.status == "error" && self.is_unread())
        }

        fn status_bucket(&self, later: &HashMap<String, u64>) -> SidebarBucket {
            if self.archived_at.is_some() {
                SidebarBucket::Archived
            } else if self.needs_input() {
                SidebarBucket::NeedsInput
            } else if self.is_active() {
                SidebarBucket::Active
            } else if self.is_unread() {
                SidebarBucket::Unread
            } else if later.contains_key(&self.id) {
                SidebarBucket::Later
            } else {
                SidebarBucket::Idle
            }
        }

        fn bucket(&self, later: &HashMap<String, u64>) -> SidebarBucket {
            if self.archived_at.is_some() {
                SidebarBucket::Archived
            } else if self.pinned_at.is_some() {
                SidebarBucket::Pinned
            } else {
                self.status_bucket(later)
            }
        }

        fn pinned_status_rank(&self, later: &HashMap<String, u64>) -> u8 {
            match self.status_bucket(later) {
                SidebarBucket::NeedsInput => 0,
                SidebarBucket::Unread => 1,
                SidebarBucket::Active => 2,
                SidebarBucket::Idle => 3,
                SidebarBucket::Later => 4,
                SidebarBucket::Archived => 5,
                SidebarBucket::Pinned => unreachable!("natural status cannot be pinned"),
            }
        }

        fn color(&self) -> Rgb {
            if self.has_pending_interaction {
                return QUESTION;
            }
            if self.status == "error" && self.is_unread() {
                return ERROR;
            }
            if self.is_active() {
                return WORKING;
            }
            if self.is_unread() {
                return ATTENTION;
            }
            IDLE
        }
    }

    fn compare_sidebar_rows(
        bucket: SidebarBucket,
        later: &HashMap<String, u64>,
        order: &HashMap<String, usize>,
        left: &ThreadEntry,
        right: &ThreadEntry,
    ) -> Ordering {
        if bucket == SidebarBucket::Pinned {
            let status_order = left
                .pinned_status_rank(later)
                .cmp(&right.pinned_status_rank(later));
            if status_order != Ordering::Equal {
                return status_order;
            }
        }
        match (order.get(&left.id), order.get(&right.id)) {
            (Some(left_index), Some(right_index)) => return left_index.cmp(right_index),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (None, None) => {}
        }
        match bucket {
            SidebarBucket::Later => later
                .get(&right.id)
                .unwrap_or(&0)
                .cmp(later.get(&left.id).unwrap_or(&0)),
            SidebarBucket::NeedsInput => right.latest_attention_at.cmp(&left.latest_attention_at),
            _ => right.updated_at.cmp(&left.updated_at),
        }
    }

    fn group_by_environment<'a>(threads: Vec<&'a ThreadEntry>) -> Vec<&'a ThreadEntry> {
        let mut groups: Vec<(String, Vec<&ThreadEntry>)> = Vec::new();
        for thread in threads {
            let key = thread.environment_id.as_ref().map_or_else(
                || format!("thread:{}", thread.id),
                |environment_id| format!("{}:{environment_id}", thread.project_id),
            );
            if let Some((_, rows)) = groups.iter_mut().find(|(group_key, _)| *group_key == key) {
                rows.push(thread);
            } else {
                groups.push((key, vec![thread]));
            }
        }
        groups.into_iter().flat_map(|(_, rows)| rows).collect()
    }

    fn slotted_threads<'a>(
        entries: &'a [ThreadEntry],
        later: &HashMap<String, u64>,
        order: &HashMap<String, usize>,
    ) -> Vec<&'a ThreadEntry> {
        let mut slots = Vec::new();
        for bucket in SIDEBAR_BUCKETS {
            let mut rows: Vec<_> = entries
                .iter()
                .filter(|thread| thread.bucket(later) == bucket)
                .collect();
            rows.sort_by(|left, right| compare_sidebar_rows(bucket, later, order, left, right));
            slots.extend(group_by_environment(rows));
        }
        slots.truncate(10);
        slots
    }

    fn status_pixels(
        entries: &[ThreadEntry],
        later: &HashMap<String, u64>,
        order: &HashMap<String, usize>,
    ) -> Vec<(u8, Rgb)> {
        let slots = slotted_threads(entries, later, order);
        let mut pixels: Vec<_> = slots
            .iter()
            .enumerate()
            .map(|(index, thread)| (index as u8, thread.color()))
            .collect();

        for index in slots.len()..10 {
            pixels.push((index as u8, OFF));
        }
        // Y/U used to be binary approve/deny controls, but bb interactions are
        // commonly free-text questions. Keep their retired LEDs cleared.
        pixels.push((IDX_Y, OFF));
        pixels.push((IDX_U, OFF));
        pixels.push((IDX_F, FOCUS_BEACON));
        pixels
    }

    fn encode_frame(pixels: &[(u8, Rgb)]) -> Result<Vec<u8>> {
        if pixels.len() > 18 {
            bail!("a Go60 frame can contain at most 18 pixels");
        }
        let mut frame = Vec::with_capacity(1 + pixels.len() * 4);
        frame.push(pixels.len() as u8);
        for (index, Rgb(r, g, b)) in pixels {
            frame.extend_from_slice(&[*index, *r, *g, *b]);
        }
        Ok(frame)
    }

    struct BleWriter {
        name_filter: String,
        characteristic: Option<Characteristic>,
        last_frame: Option<Vec<u8>>,
    }

    impl BleWriter {
        fn new(name_filter: String) -> Self {
            Self {
                name_filter,
                characteristic: None,
                last_frame: None,
            }
        }

        async fn locate(&self) -> Result<Characteristic> {
            let session = bluer::Session::new()
                .await
                .context("failed to connect to the system BlueZ service")?;
            let adapter = session
                .default_adapter()
                .await
                .context("no default Bluetooth adapter")?;
            let filter = self.name_filter.to_lowercase();

            for address in adapter.device_addresses().await? {
                let device = adapter.device(address)?;
                let name = device.name().await?.unwrap_or_default();
                if !name.to_lowercase().contains(&filter) {
                    continue;
                }
                if !device.is_connected().await? {
                    device
                        .connect()
                        .await
                        .with_context(|| format!("failed to connect to {name} ({address})"))?;
                }

                // BlueZ may report Connected before it has populated the
                // remote GATT object tree. Give service discovery a moment.
                for _ in 0..20 {
                    for service in device.services().await? {
                        if service.uuid().await? != SERVICE_UUID {
                            continue;
                        }
                        for characteristic in service.characteristics().await? {
                            if characteristic.uuid().await? == WRITE_UUID {
                                println!("bb-led-bridge: connected to {name} ({address})");
                                return Ok(characteristic);
                            }
                        }
                    }
                    sleep(Duration::from_millis(250)).await;
                }
                bail!("{name} is connected but its Go60 LED characteristic was not discovered");
            }
            bail!(
                "no paired Bluetooth device matching '{}' (pair the Go60 first)",
                self.name_filter
            )
        }

        async fn write(&mut self, pixels: &[(u8, Rgb)], force: bool) -> Result<bool> {
            let frame = encode_frame(pixels)?;
            if !force && self.last_frame.as_ref() == Some(&frame) {
                return Ok(false);
            }
            for attempt in 0..2 {
                if self.characteristic.is_none() {
                    self.characteristic = Some(self.locate().await?);
                }
                let characteristic = self.characteristic.as_ref().expect("set above");
                match characteristic.write(&frame).await {
                    Ok(()) => {
                        self.last_frame = Some(frame);
                        return Ok(true);
                    }
                    Err(error) if attempt == 0 => {
                        eprintln!("bb-led-bridge: Bluetooth write failed, reconnecting: {error}");
                        self.characteristic = None;
                    }
                    Err(error) => {
                        return Err(error).context("Bluetooth write failed after reconnect")
                    }
                }
            }
            unreachable!()
        }
    }

    #[derive(Clone)]
    struct BbClient {
        http: reqwest::Client,
        base_url: String,
    }

    impl BbClient {
        fn new(base_url: String) -> Self {
            Self {
                http: reqwest::Client::new(),
                base_url: base_url.trim_end_matches('/').to_string(),
            }
        }

        async fn threads(&self) -> Result<Vec<ThreadEntry>> {
            let response: SidebarBootstrap = self
                .http
                .get(format!("{}/api/v1/sidebar-bootstrap", self.base_url))
                .send()
                .await
                .context("could not reach bb")?
                .error_for_status()
                .context("bb rejected the sidebar-bootstrap request")?
                .json()
                .await
                .context("bb returned invalid sidebar data")?;
            Ok(response
                .projects
                .into_iter()
                .chain(std::iter::once(response.personal_project))
                .flat_map(|project| project.threads)
                .collect())
        }

        async fn later_threads(&self) -> Result<HashMap<String, u64>> {
            let response: LaterRpcResponse = self
                .http
                .post(format!(
                    "{}/api/v1/plugins/status-sidebar/rpc/listLater",
                    self.base_url
                ))
                .json(&serde_json::Value::Null)
                .send()
                .await
                .context("could not query status-sidebar Later rows")?
                .error_for_status()
                .context("status-sidebar rejected the Later request")?
                .json()
                .await
                .context("status-sidebar returned invalid Later data")?;
            Ok(response
                .result
                .rows
                .into_iter()
                .map(|row| (row.thread_id, row.placed_at))
                .collect())
        }

        async fn thread_order(&self) -> Result<HashMap<String, usize>> {
            let response: ThreadOrderRpcResponse = self
                .http
                .post(format!(
                    "{}/api/v1/plugins/status-sidebar/rpc/listThreadOrder",
                    self.base_url
                ))
                .json(&serde_json::Value::Null)
                .send()
                .await
                .context("could not query status-sidebar thread order")?
                .error_for_status()
                .context("status-sidebar rejected the thread-order request")?
                .json()
                .await
                .context("status-sidebar returned invalid thread-order data")?;
            Ok(response
                .result
                .thread_ids
                .into_iter()
                .enumerate()
                .map(|(index, thread_id)| (thread_id, index))
                .collect())
        }

        fn ws_url(&self) -> Result<String> {
            if let Some(rest) = self.base_url.strip_prefix("http://") {
                return Ok(format!("ws://{rest}/ws"));
            }
            if let Some(rest) = self.base_url.strip_prefix("https://") {
                return Ok(format!("wss://{rest}/ws"));
            }
            bail!("BB URL must start with http:// or https://")
        }
    }

    async fn refresh(client: &BbClient, ble: &mut BleWriter, force: bool) -> Result<()> {
        let threads = client.threads().await?;
        let later = match client.later_threads().await {
            Ok(later) => later,
            Err(error) => {
                eprintln!("bb-led-bridge: could not read status-sidebar Later rows: {error:#}");
                HashMap::new()
            }
        };
        let order = match client.thread_order().await {
            Ok(order) => order,
            Err(error) => {
                eprintln!("bb-led-bridge: could not read status-sidebar thread order: {error:#}");
                HashMap::new()
            }
        };
        let slots = slotted_threads(&threads, &later, &order);
        let summary = slots
            .iter()
            .enumerate()
            .map(|(index, thread)| format!("{}:{}", index + 1, color_name(thread.color())))
            .collect::<Vec<_>>()
            .join(" ");
        if ble
            .write(&status_pixels(&threads, &later, &order), force)
            .await?
        {
            println!(
                "bb-led-bridge: {} status-sidebar slot{}{}",
                slots.len(),
                if slots.len() == 1 { "" } else { "s" },
                if summary.is_empty() {
                    String::new()
                } else {
                    format!(" ({summary})")
                }
            );
        }
        Ok(())
    }

    fn message_requires_refresh(text: &str) -> bool {
        let parsed = match serde_json::from_str::<serde_json::Value>(text) {
            Ok(value) => value,
            Err(_) => return false,
        };
        if parsed.get("type").and_then(|value| value.as_str()) == Some("plugin-signal")
            && parsed.get("pluginId").and_then(|value| value.as_str()) == Some("status-sidebar")
        {
            return matches!(
                parsed.get("channel").and_then(|value| value.as_str()),
                Some("later-threads" | "thread-order")
            );
        }
        const RELEVANT_CHANGES: &[&str] = &[
            "thread-created",
            "thread-deleted",
            "interactions-changed",
            "status-changed",
            "archived-changed",
            "pin-state-changed",
            "parent-changed",
            "read-state-changed",
            "order-changed",
        ];
        Some(parsed)
            .filter(|value| {
                value.get("type").and_then(|value| value.as_str()) == Some("changed")
                    && value.get("entity").and_then(|value| value.as_str()) == Some("thread")
            })
            .and_then(|value| {
                value
                    .get("changes")
                    .and_then(|changes| changes.as_array())
                    .cloned()
            })
            .is_some_and(|changes| {
                changes.iter().any(|change| {
                    change
                        .as_str()
                        .is_some_and(|change| RELEVANT_CHANGES.contains(&change))
                })
            })
    }

    async fn run_daemon(name_filter: String, bb_url: String) -> Result<()> {
        let client = BbClient::new(bb_url);
        let ws_url = client.ws_url()?;
        let mut ble = BleWriter::new(name_filter);
        loop {
            if let Err(error) = refresh(&client, &mut ble, true).await {
                eprintln!("bb-led-bridge: initial refresh failed: {error:#}");
            }

            println!("bb-led-bridge: connecting to bb realtime at {ws_url}");
            let (mut socket, _) = match tokio_tungstenite::connect_async(&ws_url).await {
                Ok(connection) => connection,
                Err(error) => {
                    eprintln!("bb-led-bridge: realtime connection failed: {error}; retrying in 3s");
                    sleep(Duration::from_secs(3)).await;
                    continue;
                }
            };
            socket
                .send(Message::Text(
                    json!({"type":"subscribe","target":{"kind":"thread-list"}})
                        .to_string()
                        .into(),
                ))
                .await?;

            let mut repush = interval(Duration::from_secs(30));
            repush.set_missed_tick_behavior(MissedTickBehavior::Delay);
            repush.tick().await;
            let reconnect = loop {
                tokio::select! {
                    message = socket.next() => {
                        match message {
                            Some(Ok(Message::Text(text))) => {
                                if message_requires_refresh(&text) {
                                    // Coalesce the burst emitted for a single provider event.
                                    sleep(Duration::from_millis(60)).await;
                                    if let Err(error) = refresh(&client, &mut ble, false).await {
                                        eprintln!("bb-led-bridge: state refresh failed: {error:#}");
                                    }
                                }
                            }
                            Some(Ok(Message::Ping(payload))) => {
                                socket.send(Message::Pong(payload)).await?;
                            }
                            Some(Ok(Message::Close(_))) | None => break true,
                            Some(Err(error)) => {
                                eprintln!("bb-led-bridge: realtime read failed: {error}");
                                break true;
                            }
                            _ => {}
                        }
                    }
                    _ = repush.tick() => {
                        if let Err(error) = refresh(&client, &mut ble, true).await {
                            eprintln!("bb-led-bridge: periodic refresh failed: {error:#}");
                        }
                    }
                    _ = tokio::signal::ctrl_c() => break false,
                }
            };
            if !reconnect {
                return Ok(());
            }
            sleep(Duration::from_secs(3)).await;
        }
    }

    fn color_name(color: Rgb) -> &'static str {
        match color {
            IDLE => "idle",
            WORKING => "working",
            QUESTION => "question",
            ATTENTION => "unread",
            ERROR => "error",
            _ => "custom",
        }
    }

    fn parse_color(value: &str) -> Result<Rgb> {
        match value.to_ascii_lowercase().as_str() {
            "off" | "black" => Ok(OFF),
            "white" | "idle" => Ok(IDLE),
            "blue" | "working" => Ok(WORKING),
            "yellow" | "orange" | "question" => Ok(QUESTION),
            "green" | "unread" => Ok(ATTENTION),
            "red" | "error" => Ok(ERROR),
            hex if hex.starts_with('#') && hex.len() == 7 => Ok(Rgb(
                u8::from_str_radix(&hex[1..3], 16)?,
                u8::from_str_radix(&hex[3..5], 16)?,
                u8::from_str_radix(&hex[5..7], 16)?,
            )),
            _ => bail!("unknown color '{value}'"),
        }
    }

    fn parse_index(value: &str) -> Result<u8> {
        match value.to_ascii_lowercase().as_str() {
            "1" => Ok(0),
            "2" => Ok(1),
            "3" => Ok(2),
            "4" => Ok(3),
            "5" => Ok(4),
            "6" => Ok(5),
            "7" => Ok(6),
            "8" => Ok(7),
            "9" => Ok(8),
            "0" => Ok(9),
            "y" => Ok(IDX_Y),
            "u" => Ok(IDX_U),
            "f" => Ok(IDX_F),
            "all" => Ok(FILL_ALL),
            _ => bail!("unknown LED key '{value}'"),
        }
    }

    fn parse_frame_spec(spec: &str) -> Result<Vec<(u8, Rgb)>> {
        spec.split(',')
            .filter(|part| !part.trim().is_empty())
            .map(|part| {
                let (key, color) = part
                    .split_once('=')
                    .ok_or_else(|| anyhow!("frame entries must look like 1=blue"))?;
                Ok((parse_index(key.trim())?, parse_color(color.trim())?))
            })
            .collect()
    }

    fn usage() {
        eprintln!("bb-led-bridge (Linux/BlueZ)");
        eprintln!("  bb-led-bridge run [--name Go60] [--bb-url http://127.0.0.1:38886]");
        eprintln!("  bb-led-bridge frame 1=blue,2=question,F=white [--name Go60]");
        eprintln!("  bb-led-bridge demo [--name Go60]");
    }

    fn take_option(args: &mut Vec<String>, name: &str, default: String) -> Result<String> {
        if let Some(index) = args.iter().position(|arg| arg == name) {
            if index + 1 >= args.len() {
                bail!("{name} requires a value");
            }
            args.remove(index);
            Ok(args.remove(index))
        } else {
            Ok(default)
        }
    }

    pub async fn main() -> Result<()> {
        let mut args: Vec<String> = env::args().skip(1).collect();
        let command = args.first().cloned().unwrap_or_default();
        if !args.is_empty() {
            args.remove(0);
        }
        let name = take_option(&mut args, "--name", "Go60".to_string())?;

        match command.as_str() {
            "run" => {
                let bb_url = take_option(
                    &mut args,
                    "--bb-url",
                    env::var("BB_SERVER_URL")
                        .or_else(|_| env::var("BB_URL"))
                        .unwrap_or_else(|_| "http://127.0.0.1:38886".to_string()),
                )?;
                if !args.is_empty() {
                    bail!("unexpected arguments: {}", args.join(" "));
                }
                run_daemon(name, bb_url).await
            }
            "frame" => {
                let spec = args.first().context("frame requires a key=color spec")?;
                let pixels = parse_frame_spec(spec)?;
                let mut ble = BleWriter::new(name);
                ble.write(&pixels, true).await.map(|_| ())
            }
            "demo" => {
                let mut ble = BleWriter::new(name);
                for color in [WORKING, QUESTION, ATTENTION, ERROR, IDLE, OFF] {
                    ble.write(&[(FILL_ALL, color)], true).await?;
                    sleep(Duration::from_millis(700)).await;
                }
                Ok(())
            }
            _ => {
                usage();
                bail!("unknown or missing command")
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn thread(id: &str, updated_at: u64, display_status: &str) -> ThreadEntry {
            ThreadEntry {
                id: id.to_string(),
                status: if display_status == "error" {
                    "error"
                } else {
                    "idle"
                }
                .to_string(),
                project_id: "project".to_string(),
                archived_at: None,
                pinned_at: None,
                updated_at,
                environment_id: Some(format!("environment-{id}")),
                last_read_at: Some(10),
                latest_attention_at: 10,
                has_pending_interaction: false,
                activity: Activity::default(),
                runtime: Runtime {
                    display_status: display_status.to_string(),
                },
            }
        }

        #[test]
        fn slots_match_status_sidebar_sections_and_environment_groups() {
            let mut active_first = thread("active-first", 300, "active");
            active_first.environment_id = Some("shared".to_string());
            let active_middle = thread("active-middle", 200, "active");
            let mut active_grouped = thread("active-grouped", 100, "active");
            active_grouped.environment_id = Some("shared".to_string());
            let mut question = thread("question", 500, "active");
            question.has_pending_interaction = true;
            question.latest_attention_at = 500;
            let idle = thread("idle", 900, "idle");
            let entries = vec![idle, active_grouped, question, active_middle, active_first];
            let slots = slotted_threads(&entries, &HashMap::new(), &HashMap::new());
            assert_eq!(
                slots
                    .iter()
                    .map(|thread| thread.id.as_str())
                    .collect::<Vec<_>>(),
                vec![
                    "active-first",
                    "active-grouped",
                    "active-middle",
                    "question",
                    "idle"
                ]
            );
        }

        #[test]
        fn pinned_section_precedes_status_sections() {
            let active = thread("active", 1, "active");
            let mut old_pinned_idle = thread("pinned-idle", 1, "idle");
            old_pinned_idle.pinned_at = Some(1);
            let new_idle = thread("new-idle", 999, "idle");
            let entries = vec![new_idle, old_pinned_idle, active];
            let slots = slotted_threads(&entries, &HashMap::new(), &HashMap::new());
            assert_eq!(
                slots
                    .iter()
                    .map(|thread| thread.id.as_str())
                    .collect::<Vec<_>>(),
                vec!["pinned-idle", "active", "new-idle"]
            );
        }

        #[test]
        fn drag_order_is_respected_within_a_section() {
            let entries = vec![
                thread("first-by-time", 300, "active"),
                thread("second-by-time", 200, "active"),
                thread("third-by-time", 100, "active"),
            ];
            let order = HashMap::from([
                ("third-by-time".to_string(), 0),
                ("first-by-time".to_string(), 1),
                ("second-by-time".to_string(), 2),
            ]);
            let slots = slotted_threads(&entries, &HashMap::new(), &order);
            assert_eq!(
                slots
                    .iter()
                    .map(|thread| thread.id.as_str())
                    .collect::<Vec<_>>(),
                vec!["third-by-time", "first-by-time", "second-by-time"]
            );
        }

        #[test]
        fn pinned_status_groups_override_drag_order() {
            let mut pinned_idle = thread("pinned-idle", 2, "idle");
            pinned_idle.pinned_at = Some(2);
            let mut pinned_active = thread("pinned-active", 1, "active");
            pinned_active.pinned_at = Some(1);
            let entries = vec![pinned_idle, pinned_active];
            let order = HashMap::from([
                ("pinned-idle".to_string(), 0),
                ("pinned-active".to_string(), 1),
            ]);
            let slots = slotted_threads(&entries, &HashMap::new(), &order);
            assert_eq!(
                slots
                    .iter()
                    .map(|thread| thread.id.as_str())
                    .collect::<Vec<_>>(),
                vec!["pinned-active", "pinned-idle"]
            );
        }

        #[test]
        fn unread_section_precedes_active_section() {
            let active = thread("active", 2, "active");
            let mut unread = thread("unread", 1, "idle");
            unread.latest_attention_at = 11;
            let entries = vec![active, unread];
            let slots = slotted_threads(&entries, &HashMap::new(), &HashMap::new());
            assert_eq!(
                slots
                    .iter()
                    .map(|thread| thread.id.as_str())
                    .collect::<Vec<_>>(),
                vec!["unread", "active"]
            );
        }

        #[test]
        fn question_overrides_other_statuses() {
            let mut entry = thread("question", 1, "active");
            entry.has_pending_interaction = true;
            assert_eq!(entry.color(), QUESTION);
        }

        #[test]
        fn retired_y_u_interaction_leds_stay_off() {
            let mut entry = thread("question", 1, "active");
            entry.has_pending_interaction = true;
            let pixels = status_pixels(&[entry], &HashMap::new(), &HashMap::new());

            assert_eq!(
                pixels.iter().find(|(index, _)| *index == IDX_Y),
                Some(&(IDX_Y, OFF))
            );
            assert_eq!(
                pixels.iter().find(|(index, _)| *index == IDX_U),
                Some(&(IDX_U, OFF))
            );
        }

        #[test]
        fn unread_idle_is_green_and_read_idle_is_white() {
            let mut entry = thread("done", 1, "idle");
            entry.latest_attention_at = 11;
            assert_eq!(entry.color(), ATTENTION);
            entry.last_read_at = Some(11);
            assert_eq!(entry.color(), IDLE);
        }

        #[test]
        fn frame_encoding_is_firmware_wire_format() {
            assert_eq!(
                encode_frame(&[(0, WORKING), (IDX_F, IDLE)]).unwrap(),
                vec![2, 0, 0, 51, 255, 16, 10, 10, 10]
            );
        }

        #[test]
        fn realtime_filter_ignores_token_chatter_but_keeps_state_changes() {
            assert!(!message_requires_refresh(
                r#"{"type":"changed","entity":"thread","changes":["events-appended"]}"#
            ));
            assert!(message_requires_refresh(
                r#"{"type":"changed","entity":"thread","changes":["status-changed"]}"#
            ));
            assert!(message_requires_refresh(
                r#"{"type":"changed","entity":"thread","changes":["interactions-changed"]}"#
            ));
            assert!(message_requires_refresh(
                r#"{"type":"plugin-signal","pluginId":"status-sidebar","channel":"later-threads","payload":{}}"#
            ));
            assert!(message_requires_refresh(
                r#"{"type":"plugin-signal","pluginId":"status-sidebar","channel":"thread-order","payload":null}"#
            ));
        }
    }
}

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    linux::main().await
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("bb-led-bridge is Linux-only; use paseo-led-bridge on Windows");
    std::process::exit(1);
}
