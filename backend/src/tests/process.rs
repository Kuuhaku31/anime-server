//! 校验媒体进程资源预算, 取消和 stderr 截断的测试.

use super::*;

// stderr 只保留末尾有限字节, 避免长日志占满内存.
#[tokio::test]
async fn stderr_retains_only_a_bounded_tail() {
    let mut input = vec![b'a'; 6000];
    input.extend_from_slice(b"last error");
    let tail = read_tail(input.as_slice()).await.unwrap();
    assert_eq!(tail.len(), 4000);
    assert!(tail.ends_with("last error"));
}

// 编码线程先按基础预算分配, 释放后许可全部归还.
#[tokio::test]
async fn weighted_threads_start_at_base_and_return_every_permit() {
    // 多个任务不能因分别占有半份基础预算而互相等待.
    let manager = ProcessManager::new(3, 10);
    let (_sender, mut cancel) = watch::channel(false);
    let fast = manager.encoding_threads(2, 8, &mut cancel).await.unwrap();
    assert_eq!(fast.num_permits(), 8);
    let base = manager.encoding_threads(2, 8, &mut cancel).await.unwrap();
    assert_eq!(base.num_permits(), 2);
    let (stop, mut cancelled_waiter) = watch::channel(false);
    stop.send_replace(true);
    assert!(manager.encoding_threads(2, 8, &mut cancelled_waiter).await.is_err());
    drop(fast);
    drop(base);
    assert_eq!(manager.threads.available_permits(), 10);
    assert_eq!(manager.encoders.available_permits(), 2);
}

// 资源快照按当前许可计算总数, 已用和可用数量.
#[test]
fn process_usage_snapshot_reflects_current_permits() {
    let manager = ProcessManager::new(4, 16);
    let usage = manager.usage();
    assert_eq!(usage.process_slots, ResourceUsage { total: 4, used: 0, available: 4 });
    assert_eq!(usage.encoder_slots, ResourceUsage { total: 3, used: 0, available: 3 });
    assert_eq!(usage.thread_permits, ResourceUsage { total: 16, used: 0, available: 16 });
}
