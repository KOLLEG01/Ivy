//! Line-oriented adapter used only by the TypeScript/Rust differential test.
use ivy_task_board::coordination::{
    Dependencies, GraphError, Parent, blocker_satisfied, validate_blocker, validate_parent,
};
use std::collections::HashMap;
use std::future::{Future, ready};
use std::io::{self, BufRead};
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake};

struct ReadyWake;
impl Wake for ReadyWake {
    fn wake(self: Arc<Self>) {}
}
fn complete<T>(future: impl Future<Output = T>) -> T {
    let waker = Arc::new(ReadyWake).into();
    match pin!(future).poll(&mut Context::from_waker(&waker)) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("Fixture stores must complete synchronously"),
    }
}
fn optional(value: &str) -> Option<&str> {
    (value != "-").then_some(value)
}
fn id(value: &str) -> Option<u32> {
    optional(value).map(|x| x.parse().unwrap())
}
fn result(value: Result<(), GraphError<&str>>, reads: &[u32]) -> String {
    let (code, message) = match &value {
        Ok(()) => ("ok", ""),
        Err(GraphError::Read(error)) => (*error, "fixture read failed"),
        Err(error) => error.policy_error().unwrap(),
    };
    format!(
        "{code}\t{message}\t{}",
        reads
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",")
    )
}
fn main() {
    for line in io::stdin().lock().lines() {
        let line = line.unwrap();
        let f: Vec<_> = line.split('\t').collect();
        let answer = match f[0] {
            "blocker" => {
                blocker_satisfied(f[1], f[2], f[3] == "1", f[4] == "1", optional(f[5])).to_string()
            }
            "graph" | "parent" => {
                let nodes: HashMap<_, _> = f[3]
                    .split(';')
                    .filter(|x| !x.is_empty())
                    .map(|node| {
                        let fields: Vec<_> = node.split('|').collect();
                        (fields[0].parse::<u32>().unwrap(), fields)
                    })
                    .collect();
                let mut reads = Vec::new();
                let value = if f[0] == "graph" {
                    complete(validate_blocker(
                        id(f[2]).unwrap(),
                        id(f[1]).as_ref(),
                        |key| {
                            reads.push(key);
                            ready(
                                nodes
                                    .get(&key)
                                    .map(|node| Dependencies {
                                        dependencies: optional(node[1])
                                            .map(|s| {
                                                s.split(',').map(|s| s.parse().unwrap()).collect()
                                            })
                                            .unwrap_or_default(),
                                        blocker: id(node[2]),
                                    })
                                    .ok_or("not_found"),
                            )
                        },
                    ))
                } else {
                    complete(validate_parent(id(f[2]).unwrap(), id(f[1]), |key| {
                        reads.push(key);
                        ready(
                            nodes
                                .get(&key)
                                .map(|node| Parent {
                                    archived: node[1] == "1",
                                    agent_controlled: node[2] == "1",
                                    terminal: node[3] == "1",
                                    parent: id(node[4]),
                                })
                                .ok_or("not_found"),
                        )
                    }))
                };
                result(value, &reads)
            }
            _ => panic!("Unknown fixture mode"),
        };
        println!("{answer}");
    }
}
