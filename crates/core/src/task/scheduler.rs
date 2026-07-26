use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::time::Instant;

use crate::task::{TaskDefinition, TaskResult};

#[derive(Debug)]
struct PrioritizedTask {
    task: TaskDefinition,
    submitted_at: Instant,
}

impl PartialEq for PrioritizedTask {
    fn eq(&self, other: &Self) -> bool {
        self.task.priority == other.task.priority && self.submitted_at == other.submitted_at
    }
}

impl Eq for PrioritizedTask {}

impl PartialOrd for PrioritizedTask {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PrioritizedTask {
    fn cmp(&self, other: &Self) -> Ordering {
        self.task
            .priority
            .cmp(&other.task.priority)
            .then_with(|| other.submitted_at.cmp(&self.submitted_at))
    }
}

#[derive(Debug)]
pub struct TaskScheduler {
    queue: BinaryHeap<PrioritizedTask>,
    max_concurrent: usize,
    active_count: usize,
    completed: Vec<TaskResult>,
}

impl TaskScheduler {
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            queue: BinaryHeap::new(),
            max_concurrent,
            active_count: 0,
            completed: Vec::new(),
        }
    }

    pub fn submit(&mut self, task: TaskDefinition) {
        self.queue.push(PrioritizedTask {
            task,
            submitted_at: Instant::now(),
        });
    }

    pub fn can_schedule(&self) -> bool {
        self.active_count < self.max_concurrent && !self.queue.is_empty()
    }

    pub fn next(&mut self) -> Option<TaskDefinition> {
        if self.can_schedule() {
            self.active_count += 1;
            self.queue.pop().map(|pt| pt.task)
        } else {
            None
        }
    }

    pub fn complete(&mut self, result: TaskResult) {
        self.active_count = self.active_count.saturating_sub(1);
        self.completed.push(result);
    }

    pub fn pending(&self) -> usize {
        self.queue.len()
    }

    pub fn active(&self) -> usize {
        self.active_count
    }

    pub fn completed_results(&self) -> &[TaskResult] {
        &self.completed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{TaskDefinition, TaskPriority, TaskResult};
    use std::collections::HashMap;

    fn task_def(id: &str, priority: TaskPriority) -> TaskDefinition {
        TaskDefinition {
            id: id.into(),
            title: format!("Task {}", id),
            description: String::new(),
            context: String::new(),
            metadata: HashMap::new(),
            priority,
        }
    }

    #[test]
    fn test_priority_order() {
        let mut scheduler = TaskScheduler::new(2);

        scheduler.submit(task_def("low", TaskPriority::Low));
        scheduler.submit(task_def("high", TaskPriority::High));
        scheduler.submit(task_def("normal", TaskPriority::Normal));
        scheduler.submit(task_def("critical", TaskPriority::Critical));

        let t1 = scheduler.next().unwrap();
        assert_eq!(t1.priority, TaskPriority::Critical);

        let t2 = scheduler.next().unwrap();
        assert_eq!(t2.priority, TaskPriority::High);

        assert!(!scheduler.can_schedule());

        scheduler.complete(TaskResult {
            task_id: t1.id.clone(),
            success: true,
            output: "done".into(),
            sub_results: vec![],
            tokens_used: 100,
            tool_calls: 2,
        });

        let t3 = scheduler.next().unwrap();
        assert_eq!(t3.priority, TaskPriority::Normal);
    }
}
