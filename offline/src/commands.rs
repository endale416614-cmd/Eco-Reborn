//! Test commands typed in chat (`/` or `!` prefix, as SagaECO's AtCommand accepts both).
//!
//! Toggles last for the map session; a relog turns them off again.

/// Arrival tiles used when a player asks to be unstuck, per map (map ID, x, y).
// shortcut: only the loaded maps; "nearest town" needs the warp table and map changes (plan phase 0).
pub const SAFE_POINTS: [(u32, u8, u8); 2] = [
    (10_023_000, 130, 139), // Up Town: SagaECO's return-warp arrival
    (20_013_003, 63, 58),   // GM test room entrance (P10000499)
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Help,
    GodMode(Option<bool>),
    KillAll,
    Unstuck,
    Freebies(Option<bool>),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cheats {
    pub god_mode: bool,
    pub freebies: bool,
}

/// Parses a chat line; `None` means it is ordinary chat. Unknown commands come back as
/// `Some(Err(name))` so the player gets a hint instead of the text being broadcast.
pub fn parse(text: &str) -> Option<Result<Command, String>> {
    let rest = text.trim().strip_prefix(['/', '!'])?;
    let mut words = rest.split_whitespace();
    let name = words.next()?.to_ascii_lowercase();
    let toggle = match words.next().map(str::to_ascii_lowercase).as_deref() {
        Some("on") => Some(true),
        Some("off") => Some(false),
        _ => None,
    };
    Some(match name.as_str() {
        "help" | "commands" => Ok(Command::Help),
        "godmode" | "god" => Ok(Command::GodMode(toggle)),
        "killall" => Ok(Command::KillAll),
        "unstuck" => Ok(Command::Unstuck),
        "freebies" | "free" => Ok(Command::Freebies(toggle)),
        _ => Err(name),
    })
}

pub fn safe_point(map_id: u32) -> Option<(u8, u8)> {
    SAFE_POINTS
        .iter()
        .find(|(id, _, _)| *id == map_id)
        .map(|&(_, x, y)| (x, y))
}

pub const HELP: &str = "Test commands: /godmode [on|off], /killall, /unstuck, /freebies [on|off]";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands_toggles_and_plain_chat() {
        assert_eq!(parse("hello"), None);
        assert_eq!(parse("/"), None);
        assert_eq!(parse("/godmode"), Some(Ok(Command::GodMode(None))));
        assert_eq!(parse("!GodMode on"), Some(Ok(Command::GodMode(Some(true)))));
        assert_eq!(
            parse(" /freebies off "),
            Some(Ok(Command::Freebies(Some(false))))
        );
        assert_eq!(parse("/killall"), Some(Ok(Command::KillAll)));
        assert_eq!(parse("/unstuck"), Some(Ok(Command::Unstuck)));
        assert_eq!(parse("/help"), Some(Ok(Command::Help)));
        assert_eq!(parse("/fly"), Some(Err("fly".into())));
    }

    #[test]
    fn safe_points_cover_the_loaded_maps() {
        assert_eq!(safe_point(10_023_000), Some((130, 139)));
        assert_eq!(safe_point(20_013_003), Some((63, 58)));
        assert_eq!(safe_point(1), None);
    }
}
