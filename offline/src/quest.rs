use roxmltree::{Document, Node};
use std::error::Error;
use std::fs;
use std::path::Path;

const QUEST_ID: u32 = 10_000_001;
const MAP_ID: u32 = 10_025_000;
const MOB_ID: u32 = 10_000_000;
const TIME_LIMIT_SECONDS: u32 = 3_600;
const REQUIRED_COUNT: u32 = 5;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum QuestStatus {
    None,
    Open,
    Completed,
    Failed,
}

impl QuestStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "NONE",
            Self::Open => "OPEN",
            Self::Completed => "COMPLETED",
            Self::Failed => "FAILED",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LifecycleState {
    pub(crate) points: u16,
    pub(crate) fame: u32,
    pub(crate) status: QuestStatus,
    pub(crate) credited: u32,
    pub(crate) deadline: Option<u64>,
    pub(crate) last_elapsed: u32,
}

impl LifecycleState {
    pub(crate) fn new(points: u16, fame: u32) -> Self {
        Self {
            points,
            fame,
            status: QuestStatus::None,
            credited: 0,
            deadline: None,
            last_elapsed: 0,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if self.credited > REQUIRED_COUNT {
            return Err("quest progress exceeds its required count");
        }
        match self.status {
            QuestStatus::None if self.credited != 0 || self.deadline.is_some() => {
                Err("none quest state has progress or deadline")
            }
            QuestStatus::Open if self.credited >= REQUIRED_COUNT => {
                Err("open quest state has completed progress")
            }
            QuestStatus::Completed if self.credited != REQUIRED_COUNT => {
                Err("completed quest state has incomplete progress")
            }
            QuestStatus::None => Ok(()),
            _ => {
                let deadline = self.deadline.ok_or("active quest state has no deadline")?;
                if deadline < u64::from(TIME_LIMIT_SECONDS)
                    || deadline > u64::from(u32::MAX) + u64::from(TIME_LIMIT_SECONDS)
                    || deadline - u64::from(TIME_LIMIT_SECONDS) > u64::from(self.last_elapsed)
                {
                    return Err("quest deadline is outside its elapsed timeline");
                }
                if self.status == QuestStatus::Failed && u64::from(self.last_elapsed) < deadline {
                    return Err("failed quest state predates its deadline");
                }
                Ok(())
            }
        }
    }
}

pub fn replay(db: &Path, events: &Path) -> Result<String, Box<dyn Error>> {
    let definition = load_definition(db, false, false)?;
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(false)
        .from_path(events)
        .map_err(|_| "could not open event file")?;
    let (mut event_count, mut matched) = (0u64, 0u32);
    for record in reader.records() {
        let record = record.map_err(|_| "event file contains an invalid CSV record")?;
        if record.len() != 3 {
            return Err("event record must have exactly three fields".into());
        }
        event_count += 1;
        let mob = csv_id(record.get(0))?;
        let map = csv_id(record.get(1))?;
        let party_shared = match record.get(2) {
            Some("true") => true,
            Some("false") => false,
            _ => return Err("event party_shared must be true or false".into()),
        };
        if mob == MOB_ID && map == MAP_ID && !party_shared {
            matched = matched.saturating_add(1).min(definition.required);
        }
    }
    Ok(format!(
        "Headless replay of an already-open quest; no live gameplay, acceptance, expiry, rewards or persistence.\nquest_id={QUEST_ID}\nmap_id={MAP_ID}\nmob_id={MOB_ID}\nrequired_count={}\nevents_count={event_count}\ncredited_count={matched}\nprogress={matched}/{}\ncompleted={}",
        definition.required, definition.required, matched >= definition.required
    ))
}

pub fn replay_lifecycle(
    db: &Path,
    events: &Path,
    initial_points: u16,
    initial_fame: u32,
) -> Result<String, Box<dyn Error>> {
    apply_lifecycle(
        db,
        events,
        &mut LifecycleState::new(initial_points, initial_fame),
    )
}

pub(crate) fn apply_lifecycle(
    db: &Path,
    events: &Path,
    state: &mut LifecycleState,
) -> Result<String, Box<dyn Error>> {
    state.validate()?;
    let definition = load_definition(db, true, true)?;
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(false)
        .from_path(events)
        .map_err(|_| "could not open event file")?;
    let (mut event_count, mut accepted, mut cancelled, mut rejected) = (0u64, 0u64, 0u64, 0u64);
    let mut next = state.clone();
    for record in reader.records() {
        let record = record.map_err(|_| "event file contains an invalid CSV record")?;
        if record.len() != 5 {
            return Err("lifecycle event record must have exactly five fields".into());
        }
        event_count += 1;
        let elapsed = strict_u32(record.get(0).ok_or("event field is missing")?)
            .map_err(|_| "event time is invalid")?;
        if elapsed < next.last_elapsed {
            return Err("event times must be nondecreasing".into());
        }
        next.last_elapsed = elapsed;
        match record.get(1) {
            Some("accept") | Some("cancel") => {
                if record.get(2) != Some("0")
                    || record.get(3) != Some("0")
                    || record.get(4) != Some("true")
                {
                    return Err("selection confirmation payload is invalid".into());
                }
                if record.get(1) == Some("accept") {
                    if next.status == QuestStatus::None && next.points >= 1 {
                        next.points -= 1;
                        accepted += 1;
                        next.credited = 0;
                        next.status = QuestStatus::Open;
                        next.deadline = Some(u64::from(elapsed) + u64::from(TIME_LIMIT_SECONDS));
                    } else {
                        rejected += 1;
                    }
                } else if next.status == QuestStatus::Open {
                    next.status = QuestStatus::None;
                    next.credited = 0;
                    next.deadline = None;
                    next.fame = next.fame.saturating_sub(1);
                    cancelled += 1;
                } else {
                    rejected += 1;
                }
            }
            Some("tick") => {
                if record.get(2) != Some("0")
                    || record.get(3) != Some("0")
                    || record.get(4) != Some("false")
                {
                    return Err("tick event payload is invalid".into());
                }
                if next.status == QuestStatus::Open
                    && u64::from(elapsed) >= next.deadline.unwrap_or(u64::MAX)
                {
                    next.status = QuestStatus::Failed;
                }
            }
            Some("cleanup") => {
                if record.get(2) != Some("0")
                    || record.get(3) != Some("0")
                    || record.get(4) != Some("false")
                {
                    return Err("cleanup event payload is invalid".into());
                }
                if next.status == QuestStatus::Failed {
                    next.fame = next.fame.saturating_sub(1);
                    next.status = QuestStatus::None;
                    next.credited = 0;
                    next.deadline = None;
                } else {
                    rejected += 1;
                }
            }
            Some("kill") => {
                let mob = csv_id(record.get(2))?;
                let map = csv_id(record.get(3))?;
                let shared = match record.get(4) {
                    Some("true") => true,
                    Some("false") => false,
                    _ => return Err("event party_shared must be true or false".into()),
                };
                if next.status != QuestStatus::None && mob == MOB_ID && map == MAP_ID && !shared {
                    next.credited = next.credited.saturating_add(1).min(definition.required);
                    if next.status == QuestStatus::Open && next.credited >= definition.required {
                        next.status = QuestStatus::Completed;
                    }
                }
            }
            _ => return Err("event type is invalid".into()),
        }
    }
    next.validate()?;
    let report = format!(
        "Headless lifecycle replay; no live gameplay, dialogue, rewards or persistence.\nquest_id={QUEST_ID}\nmap_id={MAP_ID}\nmob_id={MOB_ID}\ntime_limit_seconds={TIME_LIMIT_SECONDS}\nevents_count={event_count}\naccepted_count={accepted}\ncancelled_count={cancelled}\nrejected_actions={rejected}\nquest_points={points}\nfame={fame}\ncredited_count={credited}\nprogress={credited}/{}\nstatus={status}",
        definition.required,
        points = next.points,
        fame = next.fame,
        credited = next.credited,
        status = next.status.as_str()
    );
    *state = next;
    Ok(report)
}

pub fn replay_timed(db: &Path, events: &Path) -> Result<String, Box<dyn Error>> {
    let definition = load_definition(db, true, false)?;
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(false)
        .from_path(events)
        .map_err(|_| "could not open event file")?;
    let (mut event_count, mut credited, mut previous_time) = (0u64, 0u32, 0u32);
    let mut status = "OPEN";
    for record in reader.records() {
        let record = record.map_err(|_| "event file contains an invalid CSV record")?;
        if record.len() != 5 {
            return Err("timed event record must have exactly five fields".into());
        }
        event_count += 1;
        let elapsed = strict_u32(record.get(0).ok_or("event field is missing")?)
            .map_err(|_| "event time is invalid")?;
        if elapsed < previous_time {
            return Err("event times must be nondecreasing".into());
        }
        previous_time = elapsed;
        match record.get(1) {
            Some("kill") => {
                let mob = csv_id(record.get(2))?;
                let map = csv_id(record.get(3))?;
                let shared = match record.get(4) {
                    Some("true") => true,
                    Some("false") => false,
                    _ => return Err("event party_shared must be true or false".into()),
                };
                if mob == MOB_ID && map == MAP_ID && !shared {
                    credited = credited.saturating_add(1).min(definition.required);
                    if status == "OPEN" && credited >= definition.required {
                        status = "COMPLETED";
                    }
                }
            }
            Some("tick") => {
                if record.get(2) != Some("0")
                    || record.get(3) != Some("0")
                    || record.get(4) != Some("false")
                {
                    return Err("tick event payload is invalid".into());
                }
                // Expiry follows the supplied callback order; the replay invents no scheduler.
                if elapsed >= TIME_LIMIT_SECONDS && status == "OPEN" {
                    status = "FAILED";
                }
            }
            _ => return Err("event type is invalid".into()),
        }
    }
    Ok(format!(
        "Headless timed replay of an already-open quest; expiry checks use explicit ticks only, with no scheduler, live gameplay, acceptance, cancellation, rewards or persistence.\nquest_id={QUEST_ID}\nmap_id={MAP_ID}\nmob_id={MOB_ID}\ntime_limit_seconds={TIME_LIMIT_SECONDS}\nevents_count={event_count}\ncredited_count={credited}\nprogress={credited}/{}\nstatus={status}",
        definition.required
    ))
}

struct Definition {
    required: u32,
}

pub(crate) fn validate_lifecycle_definition(db: &Path) -> Result<(), Box<dyn Error>> {
    load_definition(db, true, true).map(|_| ())
}

fn load_definition(db: &Path, timed: bool, lifecycle: bool) -> Result<Definition, Box<dyn Error>> {
    let path = db.join("Quests").join("QuestDB_1.xml");
    let xml = fs::read_to_string(path).map_err(|_| "could not read quest definition")?;
    let doc = Document::parse(&xml).map_err(|_| "quest definition is malformed")?;
    let root = doc.root_element();
    if root.tag_name().name() != "QuestDB"
        || root.tag_name().namespace().is_some()
        || (lifecycle && (root.attributes().len() != 1 || root.attribute("GroupID") != Some("1")))
    {
        return Err("quest definition has an unsupported structure".into());
    }
    let mut found = None;
    for quest in root
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "Quest")
    {
        if quest.tag_name().namespace().is_some() {
            return Err("namespaced quest records are unsupported".into());
        }
        let ids = fields(quest, "ID")?;
        if ids.len() != 1 {
            return Err("quest identifier is missing or duplicated".into());
        }
        if number(ids[0])? != QUEST_ID {
            continue;
        }
        if found.is_some() {
            return Err("duplicate target quest definition".into());
        }
        if value(quest, "Type")? != "HUNT"
            || number(one(quest, "MapID1")?)? != MAP_ID
            || number(one(quest, "ObjectID1")?)? != MOB_ID
            || number(one(quest, "Count1")?)? != 5
            || value(quest, "Party")? != "False"
        {
            return Err("target quest rules do not match supported definition".into());
        }
        if timed && number(one(quest, "Time")?)? != 60 {
            return Err("target quest time limit does not match supported definition".into());
        }
        if lifecycle {
            for (name, expected) in [
                ("RequiredQuestPoints", 1),
                ("DungeonID", 0),
                ("MinLV", 255),
                ("MaxLv", 255),
            ] {
                if number(one(quest, name)?)? != expected {
                    return Err(
                        "target quest lifecycle rules do not match supported definition".into(),
                    );
                }
            }
            for (name, expected) in [
                ("Job", "NONE"),
                ("JobType", "NOVICE"),
                ("Race", "NONE"),
                ("Gender", "NONE"),
            ] {
                if value(quest, name)? != expected {
                    return Err(
                        "target quest lifecycle rules do not match supported definition".into(),
                    );
                }
            }
        }
        for name in [
            "MapID2",
            "MapID3",
            "ObjectID2",
            "ObjectID3",
            "Count2",
            "Count3",
        ] {
            if let Some(field) = fields(quest, name)?.first() {
                if number(*field)? != 0 {
                    return Err("unsupported additional quest objective".into());
                }
            }
            if fields(quest, name)?.len() > 1 {
                return Err("duplicate quest objective field".into());
            }
        }
        found = Some(Definition {
            required: number(one(quest, "Count1")?)?,
        });
    }
    found.ok_or_else(|| "target quest definition is missing".into())
}

pub(crate) fn fields<'a, 'input>(
    node: Node<'a, 'input>,
    name: &str,
) -> Result<Vec<Node<'a, 'input>>, Box<dyn Error>> {
    let matches = node
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == name)
        .collect::<Vec<_>>();
    if matches.iter().any(|n| n.tag_name().namespace().is_some()) {
        return Err("namespaced target field is unsupported".into());
    }
    Ok(matches)
}

pub(crate) fn one<'a, 'input>(
    node: Node<'a, 'input>,
    name: &str,
) -> Result<Node<'a, 'input>, Box<dyn Error>> {
    let matches = fields(node, name)?;
    if matches.len() != 1 {
        return Err("required quest field is missing or duplicated".into());
    }
    Ok(matches[0])
}

fn text<'input>(node: Node<'input, 'input>) -> Result<&'input str, Box<dyn Error>> {
    if node.attributes().len() != 0 {
        return Err("quest field attributes are unsupported".into());
    }
    let mut children = node.children();
    let child = children.next().ok_or("quest field has no text")?;
    if !child.is_text() || children.next().is_some() {
        return Err("quest field text is split or malformed".into());
    }
    Ok(child.text().ok_or("quest field has no text")?.trim())
}

fn value(node: Node<'_, '_>, name: &str) -> Result<String, Box<dyn Error>> {
    Ok(text(one(node, name)?)?.to_owned())
}
pub(crate) fn number(node: Node<'_, '_>) -> Result<u32, Box<dyn Error>> {
    strict_u32(text(node)?)
}
fn strict_u32(raw: &str) -> Result<u32, Box<dyn Error>> {
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("numeric field is invalid".into());
    }
    raw.parse()
        .map_err(|_| "quest numeric field is invalid".into())
}
fn csv_id(value: Option<&str>) -> Result<u32, Box<dyn Error>> {
    strict_u32(value.ok_or("event field is missing")?).map_err(|_| "event ID is invalid".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Temp(PathBuf);
    use std::path::PathBuf;
    impl Temp {
        fn new() -> Self {
            loop {
                let path = std::env::temp_dir().join(format!(
                    "hunt-replay-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match fs::create_dir(&path) {
                    Ok(()) => {
                        fs::create_dir(path.join("Quests")).unwrap();
                        return Self(path);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("could not create isolated test directory: {error}"),
                }
            }
        }
        fn source(&self, xml: &str) {
            fs::write(self.0.join("Quests/QuestDB_1.xml"), xml).unwrap();
        }
        fn events(&self, csv: &str) -> PathBuf {
            let p = self.0.join("events.csv");
            fs::write(&p, csv).unwrap();
            p
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    const DEF: &str = "<QuestDB><Quest><ID>10000001</ID><Type>HUNT</Type><MapID1>10025000</MapID1><ObjectID1>10000000</ObjectID1><Count1>5</Count1><Party>False</Party></Quest></QuestDB>";

    #[test]
    fn supplied_source_fixture_replays_and_caps_progress() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("research/SagaECO/Configuration/DB");
        let csv = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hunt-kills.csv");
        let report = replay(&root, &csv).unwrap();
        assert!(report.contains("events_count=9\ncredited_count=5\nprogress=5/5\ncompleted=true"));
        assert!(report.contains("already-open quest"));
    }
    #[test]
    fn four_stays_open_five_completes_and_six_caps() {
        let t = Temp::new();
        t.source(DEF);
        let four = t.events("10000000,10025000,false\n".repeat(4).as_str());
        assert!(replay(&t.0, &four)
            .unwrap()
            .contains("progress=4/5\ncompleted=false"));
        let five = t.events("10000000,10025000,false\n".repeat(5).as_str());
        assert!(replay(&t.0, &five)
            .unwrap()
            .contains("progress=5/5\ncompleted=true"));
        let six = t.events("10000000,10025000,false\n".repeat(6).as_str());
        assert!(replay(&t.0, &six)
            .unwrap()
            .contains("credited_count=5\nprogress=5/5\ncompleted=true"));
        let zero = t.events("");
        assert!(replay(&t.0, &zero)
            .unwrap()
            .contains("events_count=0\ncredited_count=0\nprogress=0/5\ncompleted=false"));
    }

    #[test]
    fn wrong_mob_map_and_party_shared_events_do_not_count() {
        let t = Temp::new();
        t.source(DEF);
        let events =
            t.events("10000001,10025000,false\n10000000,10025001,false\n10000000,10025000,true\n");
        assert!(replay(&t.0, &events)
            .unwrap()
            .contains("events_count=3\ncredited_count=0\nprogress=0/5\ncompleted=false"));
    }
    #[test]
    fn rejects_malformed_events_duplicate_target_and_split_fields() {
        let t = Temp::new();
        t.source(DEF);
        for bad in [
            "10000000,10025000,yes\n",
            "4294967296,1,false\n",
            "+1,2,false\n",
            "1,2\n",
            "1,2,false,4\n",
        ] {
            assert!(replay(&t.0, &t.events(bad)).is_err());
        }
        t.source(
            &DEF.replace("10000001", "010000001")
                .replace("</QuestDB>", "<Quest><ID>10000001</ID></Quest></QuestDB>"),
        );
        assert!(replay(&t.0, &t.events("")).is_err());
        t.source(&DEF.replace("<Type>HUNT</Type>", "<Type>HUNT<!--break--></Type>"));
        assert!(replay(&t.0, &t.events("")).is_err());
        t.source(&DEF.replace("<Count1>5</Count1>", "<Count1>6</Count1>"));
        assert!(replay(&t.0, &t.events("")).is_err());
        t.source(&DEF.replace("</QuestDB>", "<Quest><Type>HUNT</Type></Quest></QuestDB>"));
        assert!(replay(&t.0, &t.events("")).is_err());
        t.source(&DEF.replace(
            "</QuestDB>",
            "<Quest xmlns='x'><ID>20000000</ID></Quest></QuestDB>",
        ));
        assert!(replay(&t.0, &t.events("")).is_err());
        t.source(&DEF.replace(
            "<MapID1>10025000</MapID1>",
            "<MapID1 bad='x'>10025000</MapID1>",
        ));
        assert!(replay(&t.0, &t.events("")).is_err());
    }
    #[test]
    fn errors_do_not_disclose_paths_or_input_values() {
        let t = Temp::new();
        t.source(DEF);
        let report = replay(&t.0, &t.events("SECRET_RAW_VALUE,1,false\n"))
            .unwrap_err()
            .to_string();
        assert!(!report.contains("SECRET_RAW_VALUE"));
        assert!(!report.contains(t.0.to_str().unwrap()));
    }

    fn timed_events(t: &Temp, rows: &str) -> String {
        replay_timed(&t.0, &t.events(rows)).unwrap()
    }

    #[test]
    fn timed_fixture_and_boundary_ordering() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("research/SagaECO/Configuration/DB");
        let events =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/timed-hunt-events.csv");
        let report = replay_timed(&root, &events).unwrap();
        assert!(report.contains("time_limit_seconds=3600\nevents_count=9\ncredited_count=5\nprogress=5/5\nstatus=COMPLETED"));

        let t = Temp::new();
        t.source(&DEF.replace("</Quest>", "<Time>60</Time></Quest>"));
        assert!(timed_events(&t, "")
            .contains("events_count=0\ncredited_count=0\nprogress=0/5\nstatus=OPEN"));
        assert!(timed_events(&t, "3599,tick,0,0,false\n").contains("status=OPEN"));
        assert!(timed_events(&t, "3600,tick,0,0,false\n").contains("status=FAILED"));
        let five = "3600,kill,10000000,10025000,false\n".repeat(5);
        assert!(
            timed_events(&t, &format!("{five}3600,tick,0,0,false\n")).contains("status=COMPLETED")
        );
        assert!(timed_events(&t, &format!("3600,tick,0,0,false\n{five}"))
            .contains("credited_count=5\nprogress=5/5\nstatus=FAILED"));
        let after_deadline = "3601,kill,10000000,10025000,false\n".repeat(5);
        assert!(timed_events(&t, &after_deadline)
            .contains("credited_count=5\nprogress=5/5\nstatus=COMPLETED"));
    }

    #[test]
    fn timed_failure_keeps_counting_without_completing_and_requires_time_field() {
        let t = Temp::new();
        t.source(&DEF.replace("</Quest>", "<Time>60</Time></Quest>"));
        assert!(timed_events(
            &t,
            "3600,tick,0,0,false\n3601,kill,10000000,10025000,false\n3602,kill,10000000,10025001,false\n"
        )
        .contains("credited_count=1\nprogress=1/5\nstatus=FAILED"));
        t.source(DEF);
        assert!(replay_timed(&t.0, &t.events("")).is_err());
        for bad_time in ["0", "60<!--x-->", "4294967296"] {
            t.source(&DEF.replace("</Quest>", &format!("<Time>{bad_time}</Time></Quest>")));
            assert!(replay_timed(&t.0, &t.events("")).is_err());
        }
        for bad_field in [
            "<Time>60</Time><Time>60</Time>",
            "<Time bad='x'>60</Time>",
            "<Time xmlns='x'>60</Time>",
        ] {
            t.source(&DEF.replace("</Quest>", &format!("{bad_field}</Quest>")));
            assert!(replay_timed(&t.0, &t.events("")).is_err());
        }
    }

    #[test]
    fn timed_input_validation_is_strict_and_errors_are_private() {
        let t = Temp::new();
        t.source(&DEF.replace("</Quest>", "<Time>60</Time></Quest>"));
        for bad in [
            "0,kill,1,2,false,extra\n",
            "0,kill,1,2\n",
            "0,other,0,0,false\n",
            "0,tick,1,0,false\n",
            "2,tick,0,0,false\n1,tick,0,0,false\n",
            "4294967296,tick,0,0,false\n",
            "+0,tick,0,0,false\n",
            "0,kill,1,2,maybe\n",
            "0,tick,0,0,false\n1,kill,1,2,false,extra\n",
            "0,kill,SECRET_RAW_VALUE,1,false\n",
        ] {
            let err = replay_timed(&t.0, &t.events(bad)).unwrap_err().to_string();
            assert!(!err.contains("SECRET_RAW_VALUE"));
            assert!(!err.contains(t.0.to_str().unwrap()));
        }
    }

    const LIFECYCLE_DEF: &str = "<QuestDB GroupID=\"1\"><Quest><ID>10000001</ID><Type>HUNT</Type><Time>60</Time><MapID1>10025000</MapID1><ObjectID1>10000000</ObjectID1><Count1>5</Count1><Party>False</Party><RequiredQuestPoints>1</RequiredQuestPoints><DungeonID>0</DungeonID><MinLV>255</MinLV><MaxLv>255</MaxLv><Job>NONE</Job><JobType>NOVICE</JobType><Race>NONE</Race><Gender>NONE</Gender></Quest></QuestDB>";

    #[test]
    fn lifecycle_fixture_and_state_refusals() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("research/SagaECO/Configuration/DB");
        let events = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/lifecycle-hunt-events.csv");
        let report = replay_lifecycle(&root, &events, 2, 1).unwrap();
        assert!(report.contains("events_count=13\naccepted_count=2\ncancelled_count=1\nrejected_actions=3\nquest_points=0\nfame=0\ncredited_count=5\nprogress=5/5\nstatus=COMPLETED"));
        assert!(report.contains("no live gameplay, dialogue, rewards or persistence"));
    }

    #[test]
    fn lifecycle_batches_continue_and_roll_back_on_errors() {
        let t = Temp::new();
        t.source(LIFECYCLE_DEF);
        let rows = "100,accept,0,0,true\n101,kill,10000000,10025000,false\n102,kill,10000000,10025000,false\n103,tick,0,0,false\n";
        let mut whole = LifecycleState::new(1, 2);
        apply_lifecycle(&t.0, &t.events(rows), &mut whole).unwrap();
        let mut split = LifecycleState::new(1, 2);
        apply_lifecycle(
            &t.0,
            &t.events("100,accept,0,0,true\n101,kill,10000000,10025000,false\n"),
            &mut split,
        )
        .unwrap();
        apply_lifecycle(
            &t.0,
            &t.events("102,kill,10000000,10025000,false\n103,tick,0,0,false\n"),
            &mut split,
        )
        .unwrap();
        assert_eq!(split, whole);

        let before = split.clone();
        assert!(apply_lifecycle(
            &t.0,
            &t.events("104,kill,10000000,10025000,false\n105,other,0,0,false\n"),
            &mut split,
        )
        .is_err());
        assert_eq!(split, before);
        assert!(apply_lifecycle(&t.0, &t.events("99,tick,0,0,false\n"), &mut split,).is_err());
        assert_eq!(split, before);
    }

    #[test]
    fn lifecycle_state_validation_checks_progress_and_deadline_timeline() {
        let mut state = LifecycleState::new(1, 0);
        assert!(state.validate().is_ok());
        state.status = QuestStatus::Open;
        state.deadline = Some(3_600);
        assert!(state.validate().is_ok());
        state.credited = REQUIRED_COUNT;
        assert!(state.validate().is_err());
        state.status = QuestStatus::Completed;
        state.credited = REQUIRED_COUNT - 1;
        assert!(state.validate().is_err());
        state.credited = REQUIRED_COUNT;
        assert!(state.validate().is_ok());
        state.last_elapsed = 3_600;
        assert!(state.validate().is_ok());
        state.status = QuestStatus::Failed;
        state.last_elapsed = 3_599;
        assert!(state.validate().is_err());
    }

    #[test]
    fn lifecycle_reaccept_resets_progress_and_deadline_is_relative() {
        let t = Temp::new();
        t.source(LIFECYCLE_DEF);
        let output = |rows: &str| replay_lifecycle(&t.0, &t.events(rows), 2, 0).unwrap();
        assert!(output("0,accept,0,0,true\n1,kill,10000000,10025000,false\n2,cancel,0,0,true\n3,accept,0,0,true\n3602,tick,0,0,false\n")
            .contains("accepted_count=2\ncancelled_count=1\nrejected_actions=0\nquest_points=0\nfame=0\ncredited_count=0\nprogress=0/5\nstatus=OPEN"));
        assert!(
            output("100,accept,0,0,true\n3699,tick,0,0,false\n3700,tick,0,0,false\n")
                .contains("status=FAILED")
        );
        assert!(
            output("4294967295,accept,0,0,true\n4294967295,tick,0,0,false\n")
                .contains("status=OPEN")
        );
        assert!(output("0,accept,0,0,true\n3600,tick,0,0,false\n").contains("status=FAILED"));
    }

    #[test]
    fn lifecycle_validates_confirmation_source_and_order() {
        let t = Temp::new();
        t.source(LIFECYCLE_DEF);
        for bad in [
            "0,accept,0,0,false\n",
            "0,accept,00,0,true\n",
            "0,cancel,0,0,false\n",
            "1,tick,0,0,true\n",
            "0,accept,0,0,true\n1,tick,0,0,false\n0,tick,0,0,false\n",
            "4294967296,tick,0,0,false\n",
            "0,kill,+1,2,false\n",
            "0,accept,SECRET,0,true\n",
        ] {
            assert!(replay_lifecycle(&t.0, &t.events(bad), 2, 1).is_err());
        }
        for invalid in [
            LIFECYCLE_DEF.replace("GroupID=\"1\"", "GroupID=\"1\" extra=\"x\""),
            LIFECYCLE_DEF.replace(
                "<RequiredQuestPoints>1</RequiredQuestPoints>",
                "<RequiredQuestPoints>2</RequiredQuestPoints>",
            ),
            LIFECYCLE_DEF.replace("<Job>NONE</Job>", "<Job>OTHER</Job>"),
            LIFECYCLE_DEF.replace(
                "<DungeonID>0</DungeonID>",
                "<DungeonID>0</DungeonID><DungeonID>0</DungeonID>",
            ),
            LIFECYCLE_DEF.replace("<MinLV>255</MinLV>", "<MinLV bad=\"x\">255</MinLV>"),
            LIFECYCLE_DEF.replace("<Race>NONE</Race>", "<Race xmlns=\"x\">NONE</Race>"),
            LIFECYCLE_DEF.replace(
                "<Gender>NONE</Gender>",
                "<Gender>NO<!-- split -->NE</Gender>",
            ),
            LIFECYCLE_DEF.replace("GroupID=\"1\"", ""),
            LIFECYCLE_DEF.replace("GroupID=\"1\"", "GroupID=\"2\""),
            LIFECYCLE_DEF.replace("GroupID=\"1\"", "xmlns:g=\"x\" g:GroupID=\"1\""),
            LIFECYCLE_DEF.replace("<MaxLv>255</MaxLv>", "<MaxLv>254</MaxLv>"),
            LIFECYCLE_DEF.replace("<JobType>NOVICE</JobType>", "<JobType>OTHER</JobType>"),
            LIFECYCLE_DEF.replace("<Gender>NONE</Gender>", "<Gender>MALE</Gender>"),
            LIFECYCLE_DEF.replace("<RequiredQuestPoints>1</RequiredQuestPoints>", ""),
        ] {
            t.source(&invalid);
            assert!(replay_lifecycle(&t.0, &t.events(""), 2, 1).is_err());
        }
    }

    #[test]
    fn lifecycle_zero_points_and_failed_state_refuse_mutations() {
        let t = Temp::new();
        t.source(LIFECYCLE_DEF);
        let p = t.events("0,accept,0,0,true\n1,accept,0,0,true\n2,tick,0,0,false\n3,cancel,0,0,true\n4,accept,0,0,true\n");
        let report = replay_lifecycle(&t.0, &p, 0, 0).unwrap();
        assert!(report.contains("accepted_count=0\ncancelled_count=0\nrejected_actions=4\nquest_points=0\nfame=0\ncredited_count=0\nprogress=0/5\nstatus=NONE"));
        let p = t.events(
            "0,accept,0,0,true\n3600,tick,0,0,false\n3601,cancel,0,0,true\n3602,accept,0,0,true\n",
        );
        let report = replay_lifecycle(&t.0, &p, 2, 1).unwrap();
        assert!(report.contains("accepted_count=1\ncancelled_count=0\nrejected_actions=2\nquest_points=1\nfame=1\ncredited_count=0\nprogress=0/5\nstatus=FAILED"));
    }

    #[test]
    fn lifecycle_tick_order_and_failed_kill_credit() {
        let t = Temp::new();
        t.source(LIFECYCLE_DEF);
        let five = "3600,kill,10000000,10025000,false\n".repeat(5);
        let before = replay_lifecycle(
            &t.0,
            &t.events(&format!("0,accept,0,0,true\n{five}3600,tick,0,0,false\n")),
            1,
            0,
        )
        .unwrap();
        assert!(before.contains("credited_count=5\nprogress=5/5\nstatus=COMPLETED"));
        let after = replay_lifecycle(
            &t.0,
            &t.events(&format!("0,accept,0,0,true\n3600,tick,0,0,false\n{five}")),
            1,
            0,
        )
        .unwrap();
        assert!(after.contains("credited_count=5\nprogress=5/5\nstatus=FAILED"));
        let invalid_kills = "3600,tick,0,0,false\n3601,kill,10000000,10025000,false\n3602,kill,10000000,10025000,false\n3603,kill,10000001,10025000,false\n3604,kill,10000000,10025001,false\n3605,kill,10000000,10025000,true\n";
        let report = replay_lifecycle(
            &t.0,
            &t.events(&format!("0,accept,0,0,true\n{invalid_kills}")),
            1,
            0,
        )
        .unwrap();
        assert!(report.contains("credited_count=2\nprogress=2/5\nstatus=FAILED"));
        let empty = replay_lifecycle(&t.0, &t.events(""), 0, 0).unwrap();
        assert!(
            empty.contains("events_count=0\naccepted_count=0") && empty.ends_with("status=NONE")
        );
    }

    #[test]
    fn lifecycle_failed_cleanup_is_source_triggered_and_reopens_acceptance() {
        let t = Temp::new();
        t.source(LIFECYCLE_DEF);
        let events = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/failed-cleanup-events.csv");
        let report = replay_lifecycle(&t.0, &events, 2, 1).unwrap();
        assert!(report.contains(
            "events_count=8\naccepted_count=2\ncancelled_count=0\nrejected_actions=3\nquest_points=0\nfame=0\ncredited_count=0\nprogress=0/5\nstatus=OPEN"
        ));
        for invalid in [
            "0,cleanup,0,0,true\n",
            "0,cleanup,1,0,false\n",
            "0,cleanup,0,0,maybe\n",
        ] {
            assert!(replay_lifecycle(&t.0, &t.events(invalid), 1, 0).is_err());
        }

        let zero_fame = replay_lifecycle(
            &t.0,
            &t.events("0,accept,0,0,true\n3600,tick,0,0,false\n3601,cleanup,0,0,false\n"),
            1,
            0,
        )
        .unwrap();
        assert!(zero_fame
            .contains("quest_points=0\nfame=0\ncredited_count=0\nprogress=0/5\nstatus=NONE"));

        let mut failed = LifecycleState::new(1, 1);
        apply_lifecycle(
            &t.0,
            &t.events(
                "0,accept,0,0,true\n3600,tick,0,0,false\n3601,kill,10000000,10025000,false\n",
            ),
            &mut failed,
        )
        .unwrap();
        assert_eq!(failed.status, QuestStatus::Failed);
        assert_eq!(failed.credited, 1);
        assert_eq!(failed.deadline, Some(3_600));
        let before = failed.clone();
        assert!(apply_lifecycle(
            &t.0,
            &t.events("3602,cleanup,0,0,false\n3603,unknown,0,0,false\n"),
            &mut failed,
        )
        .is_err());
        assert_eq!(failed, before);

        let mut completed = LifecycleState::new(1, 7);
        let five_kills = "1,kill,10000000,10025000,false\n".repeat(5);
        apply_lifecycle(
            &t.0,
            &t.events(&format!("0,accept,0,0,true\n{five_kills}")),
            &mut completed,
        )
        .unwrap();
        let before = completed.clone();
        apply_lifecycle(&t.0, &t.events("2,cleanup,0,0,false\n"), &mut completed).unwrap();
        assert_eq!(completed.points, before.points);
        assert_eq!(completed.fame, before.fame);
        assert_eq!(completed.status, QuestStatus::Completed);
        assert_eq!(completed.credited, before.credited);
        assert_eq!(completed.deadline, before.deadline);
    }

    #[test]
    fn lifecycle_rejects_malformed_rows_and_keeps_errors_private() {
        let t = Temp::new();
        t.source(LIFECYCLE_DEF);
        for bad in [
            "0,accept,0,0,true,extra\n",
            "0,accept,0,0\n",
            "0,accept,0,0,true\n1,accept,0,0,true,extra\n",
            "0,accept,0,0,true\n1,accept,0,0\n",
            "0,accept,-1,0,true\n",
            "0,accept,4294967296,0,true\n",
            "0,other,0,0,false\n",
            "0,kill,10000000,10025000,yes\n",
            "SECRET_PRIVATE_MARKER,accept,0,0,true\n",
        ] {
            let error = replay_lifecycle(&t.0, &t.events(bad), 1, 0)
                .unwrap_err()
                .to_string();
            assert!(!error.contains("SECRET_PRIVATE_MARKER"));
            assert!(!error.contains(t.0.to_str().unwrap()));
        }
    }
}
