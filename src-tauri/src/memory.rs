use serde::Serialize;
use sysinfo::System;

use crate::jobs::Log;

// Java needs memory beyond its heap for class metadata, compiled code and native buffers, which in
// a modded client often comes to around a gigabyte.
const JAVA_OVERHEAD: u64 = 1 << 30;
const GIB: f64 = (1u64 << 30) as f64;

#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum Process {
    Server,
    Client,
    SecondClient,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Launch {
    pub process: Process,
    pub heap: u64,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MemoryWarning {
    pub available: u64,
    pub needed: u64,
    pub launches: Vec<Launch>,
}

impl MemoryWarning {
    pub fn log(&self, log: &Log) {
        log.info(format!(
            "This computer has {} of memory free, and this run needs about {}. Java may run out of memory, or your computer may slow down.",
            gigabytes(self.available),
            gigabytes(self.needed)
        ));
    }
}

fn gigabytes(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / GIB)
}

pub struct Memory {
    available: u64,
    total: u64,
}

impl Memory {
    // On macOS, sysinfo counts memory macOS could free, like file caches, as available.
    pub fn now() -> Option<Self> {
        let mut system = System::new();
        system.refresh_memory();
        let memory = Self { available: system.available_memory(), total: system.total_memory() };
        (memory.total > 0).then_some(memory)
    }

    // Java's default heap is a quarter of the computer's memory.
    pub fn launch(&self, process: Process, args: &[String]) -> Launch {
        Launch { process, heap: max_heap(args).unwrap_or(self.total / 4) }
    }

    // We only advise, since free memory changes by the second and Java rarely fills its whole heap.
    pub fn check(&self, launches: Vec<Launch>) -> Option<MemoryWarning> {
        let needed = launches.iter().map(|launch| launch.heap + JAVA_OVERHEAD).sum();
        (needed > self.available).then_some(MemoryWarning { available: self.available, needed, launches })
    }
}

// Java takes the last -Xmx it's given.
pub fn max_heap(args: &[String]) -> Option<u64> {
    args.iter().rev().find_map(|arg| arg.strip_prefix("-Xmx")).and_then(parse_size)
}

fn parse_size(text: &str) -> Option<u64> {
    let split = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len());
    let (digits, unit) = text.split_at(split);
    let multiplier: u64 = match unit {
        "" => 1,
        "k" | "K" => 1 << 10,
        "m" | "M" => 1 << 20,
        "g" | "G" => 1 << 30,
        "t" | "T" => 1 << 40,
        _ => return None,
    };
    digits.parse::<u64>().ok()?.checked_mul(multiplier)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn reads_the_last_max_heap() {
        assert_eq!(max_heap(&args(&["-Xmx8G", "-Xms2G", "-Xmx6144m"])), Some(6144 << 20));
        assert_eq!(max_heap(&args(&["-Xmx1073741824"])), Some(1 << 30));
        assert_eq!(max_heap(&args(&["-Xms4G"])), None);
        assert_eq!(max_heap(&args(&["-Xmx8GB"])), None);
    }

    #[test]
    fn warns_only_when_the_run_needs_more_than_is_free() {
        let memory = Memory { available: 16 << 30, total: 32 << 30 };
        let server = memory.launch(Process::Server, &args(&["-Xmx6G"]));
        let client = memory.launch(Process::Client, &args(&["-Xmx8G"]));
        assert_eq!(memory.check(vec![server.clone(), client.clone()]), None);
        let second = memory.launch(Process::SecondClient, &args(&[]));
        assert_eq!(second.heap, 8 << 30);
        let warning = memory.check(vec![server, client, second]).unwrap();
        assert_eq!(warning.needed, 25 << 30);
    }
}
