use std::time::{Duration, Instant};

use crate::core::server::rcon::{CommandOutcome, Retransmit, ServerText, Session};

pub(crate) fn command(
    session: &mut Session,
    text: &str,
    timeout: Duration,
) -> (CommandOutcome, Vec<ServerText>) {
    let deadline = Instant::now() + timeout;
    let _ = session.pump(Duration::from_millis(20));
    session.take_messages();
    let outcome = session.command(
        text.as_bytes(),
        deadline.saturating_duration_since(Instant::now()),
        Retransmit::Never,
    );
    let capture_until = deadline.min(Instant::now() + Duration::from_millis(1500));
    let boundary = format!("Processing Command: {text}");
    let mut started = false;
    let mut messages = Vec::new();
    let mut captured_bytes = 0usize;
    loop {
        for message in session.take_messages() {
            if message.text == boundary {
                started = true;
            } else if started && message.text.starts_with("Processing Command: ") {
                started = false;
            }
            if started && messages.len() < 64 && captured_bytes + message.text.len() <= 16 * 1024 {
                captured_bytes += message.text.len();
                messages.push(message);
            } else if started && let Some(last) = messages.last_mut() {
                last.truncated = true;
            }
        }
        if Instant::now() >= capture_until {
            break;
        }
        if session
            .pump(
                capture_until
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(100)),
            )
            .is_err()
        {
            break;
        }
    }
    (outcome, messages)
}

pub(crate) fn text(messages: &[ServerText]) -> String {
    messages
        .iter()
        .map(|message| message.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::server::rcon::tests::{accepting, password, server_packet};

    #[test]
    fn delayed_native_messages_are_bound_to_the_command_after_an_empty_ack() {
        let server = accepting(|socket, peer, kind, payload| {
            if kind != 0x01 || payload[1..] != *b"#ban list 1" {
                return;
            }
            socket
                .send_to(&server_packet(0x01, &payload[..1]), peer)
                .unwrap();
            for (sequence, value) in [
                (1, "Server has no bans to list."),
                (2, "Processing Command: #ban list 1"),
                (3, "Server has no bans to list."),
            ] {
                if sequence == 2 {
                    std::thread::sleep(Duration::from_millis(450));
                }
                let mut packet = vec![sequence];
                packet.extend_from_slice(value.as_bytes());
                socket.send_to(&server_packet(0x02, &packet), peer).unwrap();
            }
        });
        let mut session =
            Session::login(server.address, &password(), Duration::from_secs(1)).unwrap();
        let (outcome, messages) = command(&mut session, "#ban list 1", Duration::from_secs(2));
        assert_eq!(outcome, CommandOutcome::Replied(Vec::new()));
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].text, "Processing Command: #ban list 1");
        assert_eq!(messages[1].text, "Server has no bans to list.");
        assert_eq!(
            server
                .received()
                .iter()
                .filter(|(kind, payload)| *kind == 1 && payload[1..] == *b"#ban list 1")
                .count(),
            1
        );
    }
}
