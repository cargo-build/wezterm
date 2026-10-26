use super::{openpty, UnixMasterPty, UnixSlavePty};
use crate::{CommandBuilder, MasterPty, PtySize};
use nix::sys::termios::{self, SetArg};
use std::io::{Read, Write};
use std::thread;
use std::time::{Duration, Instant};

fn raw_pty() -> anyhow::Result<(UnixMasterPty, UnixSlavePty)> {
    let (master, slave) = openpty(PtySize::default())?;
    let mut settings = termios::tcgetattr(&slave.fd.0)?;
    termios::cfmakeraw(&mut settings);
    termios::tcsetattr(&slave.fd.0, SetArg::TCSANOW, &settings)?;
    Ok((master, slave))
}

fn payload() -> Vec<u8> {
    (0..=u8::MAX).cycle().take(128 * 1024).collect()
}

#[test]
fn reads_output_until_slave_closes() -> anyhow::Result<()> {
    let (master, mut slave) = raw_pty()?;
    let mut reader = master.try_clone_reader()?;
    let expected = payload();
    let sent = expected.clone();
    let producer = thread::spawn(move || slave.fd.write_all(&sent));
    let mut received = Vec::new();
    reader.read_to_end(&mut received)?;
    assert_eq!(reader.read(&mut [0])?, 0, "a closed slave must continue reporting EOF");
    producer.join().expect("PTY output producer panicked")?;
    assert_eq!(received, expected, "PTY output must drain intact before EOF");
    Ok(())
}

#[test]
fn writes_input_without_losing_bytes() -> anyhow::Result<()> {
    let (master, mut slave) = raw_pty()?;
    let mut writer = master.take_writer()?;
    let expected = payload();
    let length = expected.len();
    let consumer = thread::spawn(move || {
        let mut received = vec![0; length];
        slave.fd.read_exact(&mut received)?;
        Ok::<_, std::io::Error>(received)
    });
    writer.write_all(&expected)?;
    let received = consumer.join().expect("PTY input consumer panicked")?;
    assert_eq!(received, expected, "PTY input must preserve every byte");
    Ok(())
}

#[test]
fn input_and_output_make_progress_independently() -> anyhow::Result<()> {
    let (master, mut slave) = raw_pty()?;
    let mut reader = master.try_clone_reader()?;
    let mut writer = master.take_writer()?;
    let expected = payload();
    let child_output = expected.clone();
    let child = thread::spawn(move || {
        slave.fd.write_all(&child_output)?;
        let mut input = vec![0; child_output.len()];
        slave.fd.read_exact(&mut input)?;
        Ok::<_, std::io::Error>(input)
    });
    let output = thread::spawn(move || {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        Ok::<_, std::io::Error>(bytes)
    });
    writer.write_all(&expected)?;
    assert_eq!(child.join().expect("PTY child panicked")?, expected);
    assert_eq!(output.join().expect("PTY reader panicked")?, expected);
    Ok(())
}

#[test]
fn empty_buffers_do_not_wait_for_data() -> anyhow::Result<()> {
    let (master, _slave) = raw_pty()?;
    let mut reader = master.try_clone_reader()?;
    let mut writer = master.take_writer()?;
    assert_eq!(reader.read(&mut [])?, 0);
    assert_eq!(writer.write(&[])?, 0);
    Ok(())
}

#[test]
fn writer_can_only_be_taken_once() -> anyhow::Result<()> {
    let (master, _slave) = raw_pty()?;
    let _writer = master.take_writer()?;
    assert!(master.take_writer().is_err(), "a second writer must be rejected");
    Ok(())
}

#[test]
fn resize_preserves_terminal_dimensions() -> anyhow::Result<()> {
    let (master, _slave) = raw_pty()?;
    let size = PtySize {
        rows: 41,
        cols: 132,
        pixel_width: 1056,
        pixel_height: 656,
    };
    master.resize(size)?;
    assert_eq!(master.get_size()?, size, "PTY size must include pixel dimensions");
    Ok(())
}

#[test]
fn dropping_writer_delivers_canonical_eof() -> anyhow::Result<()> {
    let (master, slave) = openpty(PtySize::default())?;
    let mut command = CommandBuilder::new("/bin/sh");
    command.args(["-c", "while IFS= read -r line; do :; done"]);
    let mut child = slave.fd.spawn_command(command)?;
    drop(slave);
    drop(master.take_writer()?);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait()? {
            assert!(status.success(), "canonical reader exited with {}", status);
            return Ok(());
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            anyhow::bail!("dropping the PTY writer did not release the canonical reader");
        }
        thread::sleep(Duration::from_millis(10));
    }
}
