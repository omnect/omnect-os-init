use std::{os::unix::process::CommandExt, process::Command, thread::sleep, time::Duration};

fn main() {
    if omnect_os_init::run().is_err() {
        sleep(Duration::from_secs(1));
        // ToDo Not sure this makes sense yet
        Command::new("/bin/bash").exec();
    }
}
