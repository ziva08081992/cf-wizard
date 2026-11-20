using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;

namespace CfWizard.MemoryPreview
{
    public sealed class Reading
    {
        public int Pid;
        public int? ParentPid;
        public long CreationFileTime100ns;
        public bool WebView2;
        public long ObservedQpcTicks;
        public ulong PrivateCommitBytes;
        public ulong WorkingSetBytes;
        public ulong LifetimePeakWorkingSetBytes;
    }

    public sealed class Issue
    {
        public string Stage;
        public int? Pid;
        public int? Win32Error;
    }

    public sealed class Tick
    {
        public Reading Primary;
        public Reading[] Descendants;
        public Issue[] Issues;
        public bool DiscoveryOk;
        public ulong? ObservedGroupPrivateCommitBytes;
    }

    public sealed class Sampler : IDisposable
    {
        private const uint QueryAndSynchronize = 0x00101000;
        private const uint WaitTimeout = 258;
        private const int NoMoreFiles = 18;
        private const long DiscoverySeconds = 1;

        [StructLayout(LayoutKind.Sequential)]
        private struct Counters
        {
            public uint Size, PageFaultCount;
            public UIntPtr PeakWorkingSet, WorkingSet, PeakPagedPool, PagedPool;
            public UIntPtr PeakNonPagedPool, NonPagedPool, Pagefile, PeakPagefile, PrivateUsage;
        }

        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        private struct ProcessEntry
        {
            public uint Size, Usage, Pid;
            public UIntPtr DefaultHeap;
            public uint Module, Threads, ParentPid;
            public int BasePriority;
            public uint Flags;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 260)]
            public string Image;
        }

        private sealed class Owned
        {
            public int Pid;
            public int? ParentPid;
            public IntPtr Handle;
            public long Created;
            public bool WebView2;
        }

        private readonly Owned primary;
        private readonly Dictionary<int, Owned> owned = new Dictionary<int, Owned>();
        private bool discoveryOk;
        private long nextDiscovery;

        public static void CheckPlatform()
        {
            if (IntPtr.Size != 8 || Marshal.SizeOf(typeof(Counters)) != 80
                || Marshal.OffsetOf(typeof(Counters), "PrivateUsage").ToInt64() != 72
                || Marshal.SizeOf(typeof(ProcessEntry)) != 568)
                throw new PlatformNotSupportedException("Windows x64 PowerShell is required.");
        }

        public Sampler(Process process)
        {
            primary = new Owned { Pid = process.Id, Handle = process.Handle };
            primary.Created = Creation(primary.Handle);
        }

        public long CreationFileTime100ns { get { return primary.Created; } }

        private static long Creation(IntPtr handle)
        {
            long created, exited, kernel, user;
            if (!GetProcessTimes(handle, out created, out exited, out kernel, out user))
                throw new Win32Exception();
            return created;
        }

        private static bool Alive(IntPtr handle)
        {
            uint result = WaitForSingleObject(handle, 0);
            if (result == WaitTimeout) return true;
            if (result == 0) return false;
            throw new Win32Exception();
        }

        private static Issue Error(string stage, int? pid, int? nativeCode)
        {
            return new Issue { Stage = stage, Pid = pid, Win32Error = nativeCode };
        }

        private void Prune(List<Issue> issues)
        {
            foreach (Owned process in new List<Owned>(owned.Values))
            {
                try { if (Alive(process.Handle)) continue; }
                catch (Win32Exception error)
                {
                    issues.Add(Error("descendant_wait", process.Pid, error.NativeErrorCode));
                }
                CloseHandle(process.Handle);
                owned.Remove(process.Pid);
            }
        }

        private static List<ProcessEntry> Snapshot()
        {
            IntPtr snapshot = CreateToolhelp32Snapshot(2, 0); // TH32CS_SNAPPROCESS
            if (snapshot == new IntPtr(-1)) throw new Win32Exception();
            try
            {
                var entry = new ProcessEntry { Size = (uint)Marshal.SizeOf(typeof(ProcessEntry)) };
                var result = new List<ProcessEntry>();
                bool more = Process32FirstW(snapshot, ref entry);
                while (more)
                {
                    result.Add(entry);
                    more = Process32NextW(snapshot, ref entry);
                }
                int error = Marshal.GetLastWin32Error();
                if (error != NoMoreFiles) throw new Win32Exception(error);
                return result;
            }
            finally { CloseHandle(snapshot); }
        }

        private void Discover(long now, List<Issue> issues)
        {
            nextDiscovery = now + DiscoverySeconds * Stopwatch.Frequency;
            discoveryOk = false;
            try
            {
                long beforeSnapshot = DateTime.UtcNow.ToFileTimeUtc();
                List<ProcessEntry> rows = Snapshot();
                HashSet<int> attempted = new HashSet<int>();
                bool complete = true;
                int previous;
                do
                {
                    previous = owned.Count;
                    foreach (ProcessEntry row in rows)
                    {
                        int pid = (int)row.Pid;
                        int parentPid = (int)row.ParentPid;
                        if (pid == 0 || pid == primary.Pid || owned.ContainsKey(pid) || attempted.Contains(pid))
                            continue;
                        Owned parent;
                        if (parentPid == primary.Pid) parent = primary;
                        else if (!owned.TryGetValue(parentPid, out parent)) continue;
                        attempted.Add(pid);
                        if (!Alive(parent.Handle)) continue;
                        IntPtr handle = OpenProcess(QueryAndSynchronize, false, row.Pid);
                        if (handle == IntPtr.Zero)
                        {
                            issues.Add(Error("descendant_open", pid, Marshal.GetLastWin32Error()));
                            complete = false;
                            continue;
                        }
                        bool keep = false;
                        try
                        {
                            long created = Creation(handle);
                            if (created < parent.Created || created > beforeSnapshot
                                || !Alive(parent.Handle) || !Alive(handle))
                                continue;
                            owned.Add(pid, new Owned {
                                Pid = pid, ParentPid = parentPid, Handle = handle, Created = created,
                                WebView2 = String.Equals(row.Image, "msedgewebview2.exe",
                                    StringComparison.OrdinalIgnoreCase)
                            });
                            keep = true;
                        }
                        catch (Win32Exception error)
                        {
                            issues.Add(Error("descendant_identity", pid, error.NativeErrorCode));
                            complete = false;
                        }
                        finally { if (!keep) CloseHandle(handle); }
                    }
                } while (owned.Count != previous);
                discoveryOk = complete;
            }
            catch (Win32Exception error)
            {
                issues.Add(Error("discovery", null, error.NativeErrorCode));
            }
        }

        private static Reading Read(Owned process, List<Issue> issues)
        {
            var data = new Counters { Size = 80 };
            if (!GetProcessMemoryInfo(process.Handle, ref data, data.Size))
            {
                issues.Add(Error("memory", process.Pid, Marshal.GetLastWin32Error()));
                return null;
            }
            return new Reading {
                Pid = process.Pid, ParentPid = process.ParentPid,
                CreationFileTime100ns = process.Created, WebView2 = process.WebView2,
                ObservedQpcTicks = Stopwatch.GetTimestamp(),
                PrivateCommitBytes = data.PrivateUsage.ToUInt64(),
                WorkingSetBytes = data.WorkingSet.ToUInt64(),
                LifetimePeakWorkingSetBytes = data.PeakWorkingSet.ToUInt64()
            };
        }

        public Tick Sample(long now)
        {
            if (!Alive(primary.Handle)) return null;
            List<Issue> issues = new List<Issue>();
            Prune(issues);
            if (now >= nextDiscovery) Discover(now, issues);
            Reading root = Read(primary, issues);
            if (!Alive(primary.Handle)) return null;
            var readings = new List<Reading>();
            bool complete = root != null && discoveryOk && issues.Count == 0;
            ulong sum = root == null ? 0 : root.PrivateCommitBytes;
            foreach (Owned process in owned.Values)
            {
                Reading next = Read(process, issues);
                if (next == null) complete = false;
                else
                {
                    readings.Add(next);
                    sum += next.PrivateCommitBytes;
                }
            }
            return new Tick {
                Primary = root, Descendants = readings.ToArray(), Issues = issues.ToArray(),
                DiscoveryOk = discoveryOk, ObservedGroupPrivateCommitBytes = complete ? (ulong?)sum : null
            };
        }

        public void Dispose()
        {
            foreach (Owned process in owned.Values) CloseHandle(process.Handle);
            owned.Clear();
        }

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool GetProcessTimes(IntPtr process, out long created, out long exited,
            out long kernel, out long user);
        [DllImport("psapi.dll", SetLastError = true)]
        private static extern bool GetProcessMemoryInfo(IntPtr process, ref Counters counts, uint size);
        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern IntPtr CreateToolhelp32Snapshot(uint flags, uint pid);
        [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode, ExactSpelling = true)]
        private static extern bool Process32FirstW(IntPtr snapshot, ref ProcessEntry entry);
        [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode, ExactSpelling = true)]
        private static extern bool Process32NextW(IntPtr snapshot, ref ProcessEntry entry);
        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool CloseHandle(IntPtr handle);
        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern uint WaitForSingleObject(IntPtr handle, uint milliseconds);
    }

    public sealed class PhaseFeed
    {
        private const int MaxLineBytes = 4096;
        private readonly FileStream stream;
        private readonly byte[] buffer = new byte[MaxLineBytes];
        private readonly byte[] line = new byte[MaxLineBytes];
        private readonly Encoding utf8 = new UTF8Encoding(false, true);
        private int length;
        private bool oversized;

        public PhaseFeed(FileStream stream)
        {
            this.stream = stream;
        }

        public string[] ReadLines()
        {
            var result = new List<string>();
            int left = 64 * 1024;
            while (left > 0 && stream.Position < stream.Length)
            {
                int read = stream.Read(buffer, 0, Math.Min(left, buffer.Length));
                if (read == 0) break;
                left -= read;
                for (int i = 0; i < read; i++)
                {
                    byte value = buffer[i];
                    if (value == 10)
                    {
                        try { result.Add(oversized ? null : utf8.GetString(line, 0, length)); }
                        catch (DecoderFallbackException) { result.Add(null); }
                        length = 0;
                        oversized = false;
                    }
                    else if (length == line.Length) oversized = true;
                    else line[length++] = value;
                }
            }
            return result.ToArray();
        }

        public bool Incomplete { get { return length != 0 || oversized || stream.Position < stream.Length; } }
    }
}
