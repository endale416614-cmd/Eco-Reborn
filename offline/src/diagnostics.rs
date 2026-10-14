use std::io;

// Only typed events cross the diagnostic boundary; never format error messages or packet bodies.
#[derive(Clone, Copy)]
pub enum Event {
    Listening,
    ClientConnected,
    HandshakeComplete,
    LoginAccepted,
    LoginRejected,
    CharacterListSent,
    CharacterCreated,
    CharacterCreateRejected,
    MapIdSent,
    LoginStatusSent,
    ServerListSent,
    MapRedirectSent,
    MapEntered,
    MapLoaded,
    PlayerMoved,
    ConnectionClosed,
    Panic,
    BatchedFrameAccepted,
}

impl Event {
    fn name(self) -> &'static str {
        match self {
            Self::Listening => "LISTENING",
            Self::ClientConnected => "CLIENT_CONNECTED",
            Self::HandshakeComplete => "HANDSHAKE_COMPLETE",
            Self::LoginAccepted => "LOGIN_ACCEPTED",
            Self::LoginRejected => "LOGIN_REJECTED",
            Self::CharacterListSent => "CHARACTER_LIST_SENT",
            Self::CharacterCreated => "CHARACTER_CREATED",
            Self::CharacterCreateRejected => "CHARACTER_CREATE_REJECTED",
            Self::MapIdSent => "MAP_ID_SENT",
            Self::LoginStatusSent => "LOGIN_STATUS_SENT",
            Self::ServerListSent => "SERVER_LIST_SENT",
            Self::MapRedirectSent => "MAP_REDIRECT_SENT",
            Self::MapEntered => "MAP_ENTERED",
            Self::MapLoaded => "MAP_LOADED",
            Self::PlayerMoved => "PLAYER_MOVED",
            Self::ConnectionClosed => "CONNECTION_CLOSED",
            Self::Panic => "PANIC",
            Self::BatchedFrameAccepted => "BATCHED_FRAME_ACCEPTED",
        }
    }
}

#[derive(Clone, Copy)]
pub enum Failure {
    Startup,
    Connection,
    Accept,
    Storage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolStage {
    FrameHeader,
    Receive,
    FrameBody,
    FramePadding,
    InnerPacket,
    FriendInitPayload,
    FriendInitOwnership,
    Dispatch,
}

pub fn protocol_failure(stage: ProtocolStage, opcode: Option<u16>) {
    #[cfg(test)]
    CAPTURED.with(|events| events.borrow_mut().push((stage, opcode)));
    println!("{}", protocol_failure_line(stage, opcode));
}

#[cfg(test)]
thread_local! {
    static CAPTURED: std::cell::RefCell<Vec<(ProtocolStage, Option<u16>)>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
pub fn take_protocol_failures() -> Vec<(ProtocolStage, Option<u16>)> {
    CAPTURED.with(|events| std::mem::take(&mut *events.borrow_mut()))
}

fn protocol_failure_line(stage: ProtocolStage, opcode: Option<u16>) -> String {
    let stage = match stage {
        ProtocolStage::FrameHeader => "FRAME_HEADER",
        ProtocolStage::Receive => "RECEIVE",
        ProtocolStage::FrameBody => "FRAME_BODY",
        ProtocolStage::FramePadding => "FRAME_PADDING",
        ProtocolStage::InnerPacket => "INNER_PACKET",
        ProtocolStage::FriendInitPayload => "FRIEND_INIT_PAYLOAD",
        ProtocolStage::FriendInitOwnership => "FRIEND_INIT_OWNERSHIP",
        ProtocolStage::Dispatch => "DISPATCH",
    };
    match (stage, opcode) {
        ("DISPATCH", Some(opcode)) => {
            format!("ECO_EVENT event=PROTOCOL_FAILURE stage={stage} opcode=0x{opcode:04x}")
        }
        _ => format!("ECO_EVENT event=PROTOCOL_FAILURE stage={stage}"),
    }
}

pub fn event(value: Event) {
    #[cfg(test)]
    if matches!(value, Event::BatchedFrameAccepted) {
        BATCH_EVENTS.with(|count| count.set(count.get() + 1));
    }
    println!("ECO_EVENT event={}", value.name());
}

#[cfg(test)]
thread_local! {
    static BATCH_EVENTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub fn take_batched_frame_events() -> usize {
    BATCH_EVENTS.with(|count| count.replace(0))
}

pub fn friend_init_id_shape(
    zero: bool,
    matches_account: bool,
    matches_owned_slot_u32: bool,
    first_byte_owned_slot: bool,
    zero_tail: bool,
) {
    #[cfg(test)]
    FRIEND_SHAPES.with(|shapes| {
        shapes.borrow_mut().push([
            zero,
            matches_account,
            matches_owned_slot_u32,
            first_byte_owned_slot,
            zero_tail,
        ])
    });
    println!(
        "ECO_EVENT event=FRIEND_INIT_ID_SHAPE zero={zero} matches_account={matches_account} matches_owned_slot_u32={matches_owned_slot_u32} first_byte_owned_slot={first_byte_owned_slot} zero_tail={zero_tail}"
    );
}

#[cfg(test)]
thread_local! {
    static FRIEND_SHAPES: std::cell::RefCell<Vec<[bool; 5]>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
pub fn take_friend_init_shapes() -> Vec<[bool; 5]> {
    FRIEND_SHAPES.with(|shapes| std::mem::take(&mut *shapes.borrow_mut()))
}

pub fn failure(value: Failure, error: &io::Error) {
    println!("{}", failure_line(value, error));
}

fn failure_line(value: Failure, error: &io::Error) -> String {
    let event = match value {
        Failure::Startup => "STARTUP_FAILED",
        Failure::Connection => "CONNECTION_ERROR",
        Failure::Accept => "ACCEPT_ERROR",
        Failure::Storage => "STORAGE_ERROR",
    };
    let kind = match error.kind() {
        io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput => "INVALID_DATA",
        io::ErrorKind::PermissionDenied => "PERMISSION_DENIED",
        io::ErrorKind::NotFound => "NOT_FOUND",
        io::ErrorKind::AddrInUse => "ADDRESS_IN_USE",
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => "TIMEOUT",
        io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted => "CONNECTION_RESET",
        io::ErrorKind::BrokenPipe => "BROKEN_PIPE",
        io::ErrorKind::AlreadyExists => "ALREADY_EXISTS",
        _ => "IO",
    };
    format!("ECO_EVENT event={event} kind={kind}")
}

pub fn unsupported(opcode: u16) {
    println!("ECO_EVENT event=UNKNOWN_OPCODE opcode=0x{opcode:04x}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arbitrary_error_details_never_enter_diagnostics() {
        let secret = "private-user PRIVATE-PC C:\\Users\\private-user\\save.sqlite 203.0.113.27 AA:BB:CC:DD:EE:FF password=secret TOKEN_SECRET CharacterSecret";
        let error = io::Error::new(io::ErrorKind::PermissionDenied, secret);
        assert_eq!(
            failure_line(Failure::Storage, &error),
            "ECO_EVENT event=STORAGE_ERROR kind=PERMISSION_DENIED"
        );
        let nested = io::Error::other(error);
        assert_eq!(
            failure_line(Failure::Startup, &nested),
            "ECO_EVENT event=STARTUP_FAILED kind=IO"
        );
    }

    #[test]
    fn protocol_failure_reports_only_static_stage_and_validated_opcode() {
        assert_eq!(
            protocol_failure_line(ProtocolStage::FrameHeader, None),
            "ECO_EVENT event=PROTOCOL_FAILURE stage=FRAME_HEADER"
        );
        assert_eq!(
            protocol_failure_line(ProtocolStage::Receive, None),
            "ECO_EVENT event=PROTOCOL_FAILURE stage=RECEIVE"
        );
        assert_eq!(
            protocol_failure_line(ProtocolStage::Dispatch, Some(0x0032)),
            "ECO_EVENT event=PROTOCOL_FAILURE stage=DISPATCH opcode=0x0032"
        );
        assert_eq!(
            protocol_failure_line(ProtocolStage::FrameHeader, Some(0x0032)),
            "ECO_EVENT event=PROTOCOL_FAILURE stage=FRAME_HEADER"
        );
        assert_eq!(
            protocol_failure_line(ProtocolStage::Receive, Some(0x0032)),
            "ECO_EVENT event=PROTOCOL_FAILURE stage=RECEIVE"
        );
    }
}
