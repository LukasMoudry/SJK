//! JoF EJK chat placeholders, expanded locally before a message is escaped and sent.
//!
//! Reference: JoF EJK `codemp/cgame/cg_consolecmds.c::CG_Say_f` at official
//! alpha commit `9822a9d6`. The command remains an ordinary protocol-26
//! `say`, `say_team`, or `tell`; only complete whitespace-delimited tokens are
//! replaced, case-insensitively, as JoF's `Cmd_Argv` loop does.

use crate::GpuState;
use sjk_client::{ChatDestination, chat_command, chat_unescape};
use sjk_shell::local_time::LocalTime;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Values {
    health: i32,
    shield: i32,
    speed: f32,
    force: u8,
    weapon: u8,
    ammo: i32,
    ping: i32,
}

impl GpuState {
    /// Expand JoF's placeholders, then apply the existing chat colour/style policy.
    pub(crate) fn prepare_chat_command(&self, command: &str, composer_style: bool) -> String {
        let expanded = self.chat_placeholder_values().map_or_else(
            || command.to_owned(),
            |values| expand_command(command, values, LocalTime::now()),
        );
        let Some(console) = &self.console else {
            return expanded;
        };
        if composer_style {
            console.color_chat_command(&expanded)
        } else {
            console.color_chat_only_command(&expanded)
        }
    }

    fn chat_placeholder_values(&self) -> Option<Values> {
        let (session, predicted) = if let Some(session) = self.resident.session.as_ref() {
            (session, None)
        } else {
            (
                self.live_session.as_ref()?,
                self.local_prediction.predicted_state(),
            )
        };
        let snapshot = session.latest_snapshot();
        let player = &snapshot.player;
        let weapon = predicted.map_or_else(|| player.weapon(), |state| state.weapon);
        let ammo_index = sjk_client::legacy_weapon_data(weapon)
            .map(|data| data.ammo_index)
            .unwrap_or(0);
        let ammo = predicted
            .and_then(|state| state.ammo.get(ammo_index).copied())
            .or_else(|| player.ammo_value(ammo_index))
            .unwrap_or(0);
        let velocity = predicted.map_or_else(|| player.velocity(), |state| state.velocity);
        Some(Values {
            health: predicted.map_or_else(|| player.health(), |state| state.health),
            shield: player.armor(),
            speed: velocity[0].hypot(velocity[1]),
            force: predicted.map_or_else(|| player.force_power(), |state| state.force_power),
            weapon,
            ammo,
            ping: snapshot
                .server_time
                .wrapping_sub(player.command_time())
                .max(0),
        })
    }
}

/// Rewrite a chat command only when its message contains a supported placeholder.
fn expand_command(command: &str, values: Values, clock: LocalTime) -> String {
    let Ok(tokens) = sjk_shell::tokenize(command) else {
        return command.to_owned();
    };
    let (destination, body) = match tokens.as_slice() {
        [name, body @ ..] if name.eq_ignore_ascii_case("say") && !body.is_empty() => {
            (ChatDestination::Global, body)
        }
        [name, body @ ..] if name.eq_ignore_ascii_case("say_team") && !body.is_empty() => {
            (ChatDestination::Team, body)
        }
        [name, slot, body @ ..] if name.eq_ignore_ascii_case("tell") && !body.is_empty() => {
            let Ok(slot) = slot.parse() else {
                return command.to_owned();
            };
            (ChatDestination::Player(slot), body)
        }
        _ => return command.to_owned(),
    };
    let joined = body.join(" ");
    let body = chat_unescape(&joined);
    let Some(expanded) = expand_text(&body, values, clock) else {
        return command.to_owned();
    };
    chat_command(destination, &expanded).unwrap_or_else(|| command.to_owned())
}

/// Preserve the message's whitespace while replacing complete JoF tokens.
fn expand_text(text: &str, values: Values, clock: LocalTime) -> Option<String> {
    let mut output = String::with_capacity(text.len());
    let mut changed = false;
    let mut start = 0;
    while start < text.len() {
        let c = text[start..]
            .chars()
            .next()
            .expect("start is a char boundary");
        if c.is_whitespace() {
            output.push(c);
            start += c.len_utf8();
            continue;
        }
        let end = text[start..]
            .char_indices()
            .find_map(|(offset, c)| c.is_whitespace().then_some(start + offset))
            .unwrap_or(text.len());
        let word = &text[start..end];
        if let Some(replacement) = replacement(word, values, clock) {
            output.push_str(&replacement);
            changed = true;
        } else {
            output.push_str(word);
        }
        start = end;
    }
    changed.then_some(output)
}

fn replacement(word: &str, values: Values, clock: LocalTime) -> Option<String> {
    let number = |value: i32| Some(value.to_string());
    if word.eq_ignore_ascii_case("%H%") {
        number(values.health)
    } else if word.eq_ignore_ascii_case("%S%") {
        number(values.shield)
    } else if word.eq_ignore_ascii_case("%U%") {
        number((values.speed + 0.5).floor() as i32)
    } else if word.eq_ignore_ascii_case("%UKM%") {
        Some(format!("{:.1}", values.speed * 0.05))
    } else if word.eq_ignore_ascii_case("%UM%") {
        Some(format!("{:.1}", values.speed * 0.031_068_55))
    } else if word.eq_ignore_ascii_case("%F%") {
        number(i32::from(values.force))
    } else if word.eq_ignore_ascii_case("%W%") {
        Some(weapon_name(values.weapon).to_owned())
    } else if word.eq_ignore_ascii_case("%A%") {
        number(values.ammo)
    } else if word.eq_ignore_ascii_case("%P%") {
        number(values.ping)
    } else if word.eq_ignore_ascii_case("%T%") {
        let hour = match clock.hour % 24 {
            0 => 12,
            hour @ 1..=12 => hour,
            hour => hour - 12,
        };
        Some(format!(
            "{hour}:{:02} {}",
            clock.minute,
            if clock.hour % 24 < 12 { "AM" } else { "PM" }
        ))
    } else if word.eq_ignore_ascii_case("%T2%") {
        Some(format!("{:02}:{:02}", clock.hour % 24, clock.minute))
    } else {
        None
    }
}

/// Names are intentionally JoF's short strings, not SJK's longer HUD labels.
fn weapon_name(weapon: u8) -> &'static str {
    match weapon {
        1 => "Stun baton",
        2 => "Melee",
        4 => "Pistol",
        5 => "E11",
        6 => "Sniper",
        7 => "Bowcaster",
        8 => "Repeater",
        9 => "Demp2",
        10 => "Flechette",
        11 => "Rocket",
        12 => "Thermal",
        13 => "Tripmine",
        14 => "Detpack",
        15 => "Concussion rifle",
        16 => "Bryar",
        _ => "Saber",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values() -> Values {
        Values {
            health: 73,
            shield: 41,
            speed: 321.6,
            force: 88,
            weapon: 15,
            ammo: 6,
            ping: 27,
        }
    }

    fn clock(hour: u8, minute: u8) -> LocalTime {
        LocalTime {
            year: 2026,
            month: 10,
            day: 17,
            weekday: 6,
            hour,
            minute,
            second: 0,
        }
    }

    #[test]
    fn every_jof_placeholder_has_its_value() {
        let text = "%H% %S% %U% %UKM% %UM% %F% %W% %A% %P% %T% %T2%";
        assert_eq!(
            expand_text(text, values(), clock(13, 5)).as_deref(),
            Some("73 41 322 16.1 10.0 88 Concussion rifle 6 27 1:05 PM 13:05")
        );
    }

    #[test]
    fn matching_is_case_insensitive_but_only_for_complete_words() {
        assert_eq!(
            expand_text("  %h%\tHP=%H% %S%! x", values(), clock(0, 7)).as_deref(),
            Some("  73\tHP=%H% %S%! x")
        );
        assert_eq!(
            expand_text("^2Health: %H% ^5Shield: %S%", values(), clock(0, 7)).as_deref(),
            Some("^2Health: 73 ^5Shield: 41")
        );
        assert_eq!(expand_text("HP=%H%", values(), clock(0, 7)), None);
        assert_eq!(expand_text("^2%H%", values(), clock(0, 7)), None);
    }

    #[test]
    fn commands_keep_the_destination_and_reapply_chat_escaping() {
        assert_eq!(
            expand_command("say_team status %H% %", values(), clock(0, 7)),
            "say_team \"status 73 °/.\""
        );
        assert_eq!(
            expand_command("tell 4 \"%W% %A%\"", values(), clock(12, 0)),
            "tell 4 \"Concussion rifle 6\""
        );
        assert_eq!(
            expand_command("say HP=%H%", values(), clock(12, 0)),
            "say HP=%H%"
        );
    }

    #[test]
    fn weapon_names_match_jof_including_the_saber_fallback() {
        let names = [
            (1, "Stun baton"),
            (2, "Melee"),
            (4, "Pistol"),
            (5, "E11"),
            (6, "Sniper"),
            (7, "Bowcaster"),
            (8, "Repeater"),
            (9, "Demp2"),
            (10, "Flechette"),
            (11, "Rocket"),
            (12, "Thermal"),
            (13, "Tripmine"),
            (14, "Detpack"),
            (15, "Concussion rifle"),
            (16, "Bryar"),
        ];
        for (weapon, name) in names {
            assert_eq!(weapon_name(weapon), name);
        }
        assert_eq!(weapon_name(3), "Saber");
        assert_eq!(weapon_name(99), "Saber");
    }
}
