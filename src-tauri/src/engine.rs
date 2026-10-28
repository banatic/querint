//! Hold/release state machine.
//!
//! A held printer has its queue paused, so jobs stay in the local spooler.
//! Releasing jobs pauses every other job individually, then resumes the queue;
//! once the released jobs are gone the queue is paused again and the
//! per-job pauses are lifted.

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use windows::Win32::System::SystemInformation::GetLocalTime;

use crate::settings::Settings;
use crate::spooler::{self, JobCommand};

const JOB_HIDDEN: u32 = spooler::JOB_DELETING | spooler::JOB_DELETED | spooler::JOB_PRINTED | spooler::JOB_COMPLETE;
const JOB_STUCK: u32 =
    spooler::JOB_ERROR | spooler::JOB_OFFLINE | spooler::JOB_PAPEROUT | spooler::JOB_BLOCKED | spooler::JOB_USER_INTERVENTION;

pub struct PrinterEntry {
    pub name: String,
    pub host: (String, u16),
    paused: bool,
}

#[derive(Serialize, Clone, PartialEq)]
pub struct JobView {
    id: u32,
    name: String,
    pages: u32,
    time: String,
    state: &'static str,
}

#[derive(Serialize, Clone, PartialEq)]
pub struct PrinterView {
    name: String,
    offline: bool,
    hold: bool,
    jobs: Vec<JobView>,
}

#[derive(Serialize, Clone, PartialEq)]
pub struct Choice {
    name: String,
    managed: bool,
}

#[derive(Serialize, Clone, PartialEq, Default)]
pub struct Snapshot {
    printers: Vec<PrinterView>,
    choices: Vec<Choice>,
    autostart: bool,
    notify: bool,
    pub held: usize,
    pub active: usize,
}

#[derive(Default)]
pub struct Engine {
    pub settings: Settings,
    pub printers: Vec<PrinterEntry>,
    pub reach: HashMap<String, bool>,
    jobs: HashMap<String, Vec<spooler::Job>>,
    released: HashMap<String, HashSet<u32>>,
    /// While bypassing, jobs stopped with `stop` stay held; anything else goes straight out.
    kept: HashMap<String, HashSet<u32>>,
    port_cache: HashMap<String, Option<(String, u16)>>,
}

impl Engine {
    pub fn new(settings: Settings) -> Self {
        Self { settings, ..Default::default() }
    }

    /// Managed printers plus unmanaged ones we still hold jobs for.
    pub fn is_tracked(&self, name: &str) -> bool {
        self.settings.managed.iter().any(|m| m == name) || self.settings.held_by_us.iter().any(|m| m == name)
    }

    pub fn tick(&mut self) {
        // Acting on a failed read could hand back a queue that still holds jobs; wait for the next tick.
        let Some(list) = spooler::list_printers() else { return };
        let mut cache = std::mem::take(&mut self.port_cache);
        self.printers = list
            .into_iter()
            .filter_map(|p| {
                let host = spooler::port_host(&p.port, &mut cache)?;
                Some(PrinterEntry { name: p.name, host, paused: p.paused })
            })
            .collect();
        self.port_cache = cache;

        let mut settings_changed = false;
        let names: Vec<(String, bool)> = self
            .printers
            .iter()
            .filter(|p| self.is_tracked(&p.name))
            .map(|p| (p.name.clone(), p.paused))
            .collect();

        for (name, paused) in names {
            let Some(jobs) = spooler::list_jobs(&name) else { continue };
            let jobs: Vec<spooler::Job> = jobs.into_iter().filter(|j| j.status & JOB_HIDDEN == 0).collect();
            let exists = |id: &u32| jobs.iter().any(|j| j.id == *id);
            let released = self.released.entry(name.clone()).or_default();
            released.retain(exists);
            let kept = self.kept.entry(name.clone()).or_default();
            kept.retain(exists);
            let bypass = self.settings.bypass.contains(&name);
            if bypass {
                released.extend(jobs.iter().map(|j| j.id).filter(|id| !kept.contains(id)));
            }

            if released.is_empty() && !bypass {
                let managed = self.settings.managed.contains(&name);
                if managed || !jobs.is_empty() {
                    if !paused {
                        spooler::set_paused(&name, true);
                    }
                    if !self.settings.held_by_us.contains(&name) {
                        self.settings.held_by_us.push(name.clone());
                        settings_changed = true;
                    }
                } else {
                    // Unmanaged and drained: hand the queue back.
                    if paused {
                        spooler::set_paused(&name, false);
                    }
                    self.settings.held_by_us.retain(|n| n != &name);
                    settings_changed = true;
                }
                // Queue is paused again, so job-level pauses are no longer needed.
                for j in jobs.iter().filter(|j| j.status & spooler::JOB_PAUSED != 0) {
                    spooler::control_job(&name, j.id, JobCommand::Resume);
                }
            } else {
                // Only released jobs may run while the queue is open. Reads the spooler's own
                // pause flag, so pauses left behind by an earlier run are handled too.
                for j in &jobs {
                    let job_paused = j.status & spooler::JOB_PAUSED != 0;
                    let cmd = match released.contains(&j.id) {
                        true if job_paused => JobCommand::Resume,
                        false if !job_paused => JobCommand::Pause,
                        _ => continue,
                    };
                    spooler::control_job(&name, j.id, cmd);
                }
                if paused {
                    spooler::set_paused(&name, false);
                }
            }
            self.jobs.insert(name, jobs);
        }
        self.jobs.retain(|n, _| self.settings.managed.contains(n) || self.settings.held_by_us.contains(n));
        if settings_changed {
            self.settings.save();
        }
    }

    pub fn release(&mut self, printer: &str, job: Option<u32>) {
        let held: Vec<u32> = self
            .jobs
            .get(printer)
            .map(|jobs| jobs.iter().map(|j| j.id).filter(|id| job.is_none_or(|want| want == *id)).collect())
            .unwrap_or_default();
        self.released.entry(printer.to_string()).or_default().extend(held);
    }

    pub fn release_all(&mut self) {
        let names: Vec<String> = self.jobs.keys().cloned().collect();
        for n in names {
            self.release(&n, None);
        }
    }

    /// Jobs already being sent keep going; everything else returns to held.
    /// The spooler reports a job as printing even while it can't reach the printer,
    /// so on an offline printer those are pulled back and restarted from the top.
    pub fn stop(&mut self, printer: &str) {
        let offline = self.reach.get(printer) == Some(&false);
        let printing: HashSet<u32> = self
            .jobs
            .get(printer)
            .map(|jobs| jobs.iter().filter(|j| j.status & spooler::JOB_PRINTING != 0).map(|j| j.id).collect())
            .unwrap_or_default();
        let kept = self.kept.entry(printer.to_string()).or_default();
        let mut stopped = Vec::new();
        if let Some(rel) = self.released.get_mut(printer) {
            rel.retain(|id| {
                let keep = printing.contains(id) && !offline;
                if !keep {
                    kept.insert(*id);
                    stopped.push(*id);
                }
                keep
            });
        }
        let restart: Vec<u32> = stopped.into_iter().filter(|id| printing.contains(id)).collect();
        if !restart.is_empty() {
            spooler::set_paused(printer, true);
            for id in restart {
                spooler::control_job(printer, id, JobCommand::Restart);
            }
        }
    }

    pub fn stop_all(&mut self) {
        let names: Vec<String> = self.released.keys().cloned().collect();
        for n in names {
            self.stop(&n);
        }
    }

    pub fn delete(&mut self, printer: &str, job: u32) {
        spooler::control_job(printer, job, JobCommand::Delete);
        if let Some(rel) = self.released.get_mut(printer) {
            rel.remove(&job);
        }
    }

    pub fn set_bypass(&mut self, printer: &str, on: bool) {
        self.settings.bypass.retain(|n| n != printer);
        if on {
            // Lifting the hold sends what was held too.
            self.settings.bypass.push(printer.to_string());
            self.kept.remove(printer);
            self.release(printer, None);
        } else {
            self.kept.remove(printer);
        }
        self.settings.save();
    }

    pub fn set_managed(&mut self, printer: &str, managed: bool) {
        self.settings.managed.retain(|n| n != printer);
        if managed {
            self.settings.managed.push(printer.to_string());
        } else {
            // Otherwise a bypassed printer would never be handed back.
            self.set_bypass(printer, false);
        }
        self.settings.save();
    }

    pub fn held_count(&self, printer: &str) -> usize {
        let rel = self.released.get(printer);
        self.jobs
            .get(printer)
            .map(|jobs| jobs.iter().filter(|j| rel.is_none_or(|r| !r.contains(&j.id))).count())
            .unwrap_or(0)
    }

    pub fn snapshot(&self) -> Snapshot {
        let today = unsafe { GetLocalTime() };
        let mut snap = Snapshot {
            autostart: self.settings.autostart,
            notify: self.settings.notify,
            ..Default::default()
        };
        for p in &self.printers {
            snap.choices.push(Choice { name: p.name.clone(), managed: self.settings.managed.contains(&p.name) });
            let Some(jobs) = self.jobs.get(&p.name) else { continue };
            if !self.settings.managed.contains(&p.name) && jobs.is_empty() {
                continue;
            }
            let offline = self.reach.get(&p.name) == Some(&false);
            let empty = HashSet::new();
            let released = self.released.get(&p.name).unwrap_or(&empty);
            let jobs = jobs
                .iter()
                .map(|j| {
                    let state = if !released.contains(&j.id) {
                        "held"
                    } else if offline || j.status & JOB_STUCK != 0 {
                        "waiting"
                    } else if j.status & spooler::JOB_PRINTING != 0 {
                        "printing"
                    } else {
                        "queued"
                    };
                    match state {
                        "held" => snap.held += 1,
                        _ => snap.active += 1,
                    }
                    let t = &j.submitted;
                    let time = if (t.wYear, t.wMonth, t.wDay) == (today.wYear, today.wMonth, today.wDay) {
                        format!("{}:{:02}", t.wHour, t.wMinute)
                    } else {
                        format!("{}/{} {}:{:02}", t.wMonth, t.wDay, t.wHour, t.wMinute)
                    };
                    JobView { id: j.id, name: j.document.clone(), pages: j.pages, time, state }
                })
                .collect();
            snap.printers.push(PrinterView {
                name: p.name.clone(),
                offline,
                hold: !self.settings.bypass.contains(&p.name),
                jobs,
            });
        }
        snap
    }
}

#[cfg(test)]
mod tests {
    //! Live test against a local TCP/IP printer; see the comment on `live_hold_release`.
    use super::*;
    use std::process::Command;
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    const PRINTER: &str = "Querint 테스트";
    static RECEIVED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn submit(text: &str) {
        let script = format!("'{text}' | Out-Printer -Name '{PRINTER}'");
        Command::new("powershell").args(["-NoProfile", "-Command", &script]).status().unwrap();
    }

    fn jobs(e: &Engine) -> Vec<spooler::Job> {
        e.jobs.get(PRINTER).cloned().unwrap_or_default()
    }

    fn printer_paused() -> bool {
        spooler::list_printers().unwrap().into_iter().find(|p| p.name == PRINTER).unwrap().paused
    }

    fn tick_until(e: &mut Engine, secs: u64, mut done: impl FnMut(&Engine) -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(secs) {
            e.tick();
            if done(e) {
                return true;
            }
            sleep(Duration::from_millis(300));
        }
        false
    }

    /// Plays the printer: accepts RAW jobs on 127.0.0.1:9101 and discards them.
    fn spawn_sink() {
        let listener = std::net::TcpListener::bind("127.0.0.1:9101").unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let _ = std::io::copy(&mut stream, &mut std::io::sink());
                RECEIVED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                sleep(Duration::from_millis(1500));
            }
        });
    }

    /// Needs an elevated shell and a printer named PRINTER on a Standard TCP/IP port to 127.0.0.1:9101.
    /// Run with `cargo test -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_hold_release() {
        std::env::set_var("APPDATA", std::env::temp_dir().join("querint-test"));
        spawn_sink();
        let mut e = Engine::new(Settings { managed: vec![PRINTER.into()], ..Default::default() });

        e.tick();
        assert!(printer_paused(), "managed printer should be paused");

        submit("one");
        submit("two");
        submit("three");
        assert!(tick_until(&mut e, 20, |e| jobs(e).len() == 3), "3 jobs should be held");
        sleep(Duration::from_secs(2));
        e.tick();
        assert_eq!(jobs(&e).len(), 3, "held jobs must not print");

        // Release one job: only it leaves, the queue is paused again afterwards.
        // A job pause left behind by an earlier run must not keep it stuck.
        let first = jobs(&e)[0].id;
        spooler::control_job(PRINTER, first, JobCommand::Pause);
        e.release(PRINTER, Some(first));
        e.tick();
        assert!(!printer_paused(), "queue resumes while a job is released");
        assert!(tick_until(&mut e, 30, |e| !jobs(e).iter().any(|j| j.id == first)), "released job should print");
        e.tick();
        assert_eq!(jobs(&e).len(), 2, "other jobs stay");
        assert!(printer_paused(), "queue paused again after release");

        // Release all, then stop immediately: the job already sending finishes, the rest returns to held.
        e.release(PRINTER, None);
        e.tick();
        e.stop(PRINTER);
        e.tick();
        assert!(tick_until(&mut e, 30, |_| printer_paused()), "queue paused after stop");
        let left = jobs(&e).len();
        println!("after stop: {left} job(s) held");
        assert!(left >= 1, "stop should keep at least one job held");

        for j in jobs(&e) {
            e.delete(PRINTER, j.id);
        }
        assert!(tick_until(&mut e, 10, |e| jobs(e).is_empty()), "delete clears the queue");

        // Hold off for this printer: what was held goes out, and so does a new job.
        submit("held");
        assert!(tick_until(&mut e, 20, |e| jobs(e).len() == 1), "job held before bypass");
        let received = || RECEIVED.load(std::sync::atomic::Ordering::SeqCst);
        let before = received();
        e.set_bypass(PRINTER, true);
        assert!(tick_until(&mut e, 30, |e| jobs(e).is_empty()), "held job prints when hold is lifted");
        submit("through");
        assert!(tick_until(&mut e, 30, |e| received() >= before + 2 && jobs(e).is_empty()), "new job goes straight out");
        e.set_bypass(PRINTER, false);
        e.tick();
        assert!(printer_paused(), "hold back on");

        // Unmanaging an empty printer hands the queue back.
        e.set_managed(PRINTER, false);
        e.tick();
        assert!(!printer_paused(), "unmanaged printer resumes");
    }

    /// Released while nothing listens on :9101, then stopped. Run alone: `cargo test live_offline_stop -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_offline_stop() {
        std::env::set_var("APPDATA", std::env::temp_dir().join("querint-test"));
        let mut e = Engine::new(Settings { managed: vec![PRINTER.into()], ..Default::default() });
        e.reach.insert(PRINTER.into(), false);
        e.tick();
        submit("offline");
        assert!(tick_until(&mut e, 20, |e| jobs(e).len() == 1), "job held");
        let id = jobs(&e)[0].id;
        e.release(PRINTER, None);
        for _ in 0..12 {
            e.tick();
            sleep(Duration::from_millis(500));
        }
        let state = |e: &Engine| e.snapshot().printers.iter().flat_map(|p| p.jobs.clone()).find(|j| j.id == id).map(|j| j.state);
        println!("released: status {:#x} state {:?}", jobs(&e)[0].status, state(&e));
        e.stop(PRINTER);
        assert!(tick_until(&mut e, 15, |e| state(e) == Some("held") && printer_paused()), "stop re-holds an unsendable job");
        sleep(Duration::from_secs(3));
        e.tick();
        println!("stopped: status {:#x} state {:?} paused {}", jobs(&e)[0].status, state(&e), printer_paused());
        assert_eq!(state(&e), Some("held"), "stays held");
        e.delete(PRINTER, id);
        assert!(tick_until(&mut e, 10, |e| jobs(e).is_empty()), "cleanup");
    }
}
