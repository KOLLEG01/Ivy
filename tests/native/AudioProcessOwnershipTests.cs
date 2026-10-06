using System;
using System.Collections.Generic;
using Ivy.PhoneBridge;

public static class AudioProcessOwnershipTests {
    const string Image = @"C:\Uis\ChatGPT.exe";
    static readonly DesktopIdentity Root = new DesktopIdentity(10, 100, Image, "OpenAI.Codex_family!UI");
    sealed class Processes : IAudioProcessSnapshot {
        public readonly Dictionary<int, AudioProcessRecord> Values = new Dictionary<int, AudioProcessRecord>();
        public readonly Dictionary<int, int> Reads = new Dictionary<int, int>();
        public Action<int, int> BeforeRead;
        public int Disposals;
        public AudioProcessRecord Read(int pid) {
            int count; Reads.TryGetValue(pid, out count); Reads[pid] = ++count;
            if (BeforeRead != null) BeforeRead(pid, count);
            AudioProcessRecord value; Values.TryGetValue(pid, out value); return value;
        }
        public void Dispose() { Disposals++; }
    }
    static Processes Tree() {
        var value = new Processes();
        value.Values[10] = new AudioProcessRecord(10, 1, 3, 100, Image);
        value.Values[20] = new AudioProcessRecord(20, 10, 3, 200, Image);
        value.Values[30] = new AudioProcessRecord(30, 20, 3, 300, Image);
        return value;
    }
    static void Check(bool value, string message) { if (!value) throw new Exception(message); }
    static void Result(Processes processes, int pid, string expected) {
        var result = AudioProcessOwnership.Read(Root, pid, () => true, () => processes);
        Check(result.state == expected && result.pid == pid && processes.Disposals == 1, expected + " with released observation");
    }
    public static void Run() {
        Result(Tree(), 10, "owned"); Result(Tree(), 30, "owned");
        var tree = Tree(); tree.Values[20] = new AudioProcessRecord(20, 10, 4, 200, Image); Result(tree, 30, "foreign");
        tree = Tree(); tree.Values[20] = new AudioProcessRecord(20, 10, 3, 200, @"C:\Other\ChatGPT.exe"); Result(tree, 30, "foreign");
        tree = Tree(); tree.Values[20] = new AudioProcessRecord(20, 0, 3, 200, Image); Result(tree, 30, "foreign");
        tree = Tree(); tree.Values[20] = new AudioProcessRecord(20, 10, 3, 400, Image); Result(tree, 30, "unavailable");
        tree = Tree(); tree.Values[20] = new AudioProcessRecord(20, 30, 3, 300, Image); Result(tree, 30, "unavailable");
        tree = Tree(); tree.Values.Remove(20); Result(tree, 30, "unavailable");
        tree = Tree(); tree.Values[10] = new AudioProcessRecord(10, 1, 3, 101, Image); Result(tree, 30, "unavailable");
        tree = Tree(); var changed = tree;
        tree.BeforeRead = (pid, count) => { if (pid == 30 && count == 2) changed.Values[30] = new AudioProcessRecord(30, 20, 3, 301, Image); };
        Result(tree, 30, "unavailable");
        tree = Tree(); tree.BeforeRead = (pid, count) => { if (pid == 20) throw new Exception("process access lost"); }; Result(tree, 30, "unavailable");
        tree = Tree(); int rootReads = 0;
        Check(AudioProcessOwnership.Read(Root, 30, () => ++rootReads == 1, () => tree).state == "unavailable" && tree.Disposals == 1,
            "Desktop replaced after ancestry read");
        bool opened = false;
        Check(AudioProcessOwnership.Read(Root, 30, () => false, () => { opened = true; return Tree(); }).state == "unavailable" && !opened,
            "stale root refuses traversal");
        tree = Tree();
        for (int pid = 100; pid < 165; pid++) tree.Values[pid] = new AudioProcessRecord(pid, pid + 1, 3, 300, Image);
        tree.Values[165] = new AudioProcessRecord(165, 10, 3, 300, Image);
        Result(tree, 100, "unavailable");
        Console.WriteLine("audio_process_ownership_unit_passed: fake ancestry only; exact root/path/session, PID reuse, cycles, races and bounded traversal");
    }
}
