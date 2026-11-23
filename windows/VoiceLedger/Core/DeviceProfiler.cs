using System;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text.Json;
using Microsoft.Win32.SafeHandles;

namespace VoiceLedger.Core;

internal static class DeviceProfiler
{
    internal static bool Capture(string directory)
    {
        if (!Path.IsPathFullyQualified(directory)) return false;
        try
        {
            Directory.CreateDirectory(directory);
            var memory = new MemoryStatus { Length = (uint)Marshal.SizeOf<MemoryStatus>() };
            bool memoryKnown = GlobalMemoryStatusEx(ref memory) && memory.TotalPhysical != 0;
            bool powerKnown = GetSystemPowerStatus(out PowerStatus power);
            bool hasBattery = powerKnown && (power.BatteryFlag & 128) == 0;
            using var process = Process.GetCurrentProcess();
            long privateBytes = process.PrivateMemorySize64;
            var measurement = new
            {
                id = Guid.NewGuid(),
                device_id = Guid.NewGuid(),
                observed_at_unix_ms = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds(),
                platform = "windows",
                os_version = Environment.OSVersion.Version.ToString(),
                logical_cpu_count = Environment.ProcessorCount,
                total_memory_bytes = memoryKnown ? ToCount(memory.TotalPhysical) : null,
                available_memory_bytes = memoryKnown ? ToCount(memory.AvailablePhysical) : null,
                process_memory_bytes = privateBytes > 0 ? (long?)privateBytes : null,
                low_memory = LowMemory(),
                storage_available_bytes = GetDiskFreeSpaceEx(directory, out ulong available, out _, out _)
                    ? ToCount(available) : null,
                power_saver = powerKnown && power.Saver <= 1 ? (bool?)(power.Saver == 1) : null,
                battery_percent = hasBattery && power.BatteryPercent <= 100 ? (byte?)power.BatteryPercent : null,
            };
            NativeCore.SaveDeviceProfile(directory, JsonSerializer.SerializeToUtf8Bytes(measurement));
            return true;
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException
            or CoreException or TypeInitializationException or EntryPointNotFoundException)
        {
            return false;
        }
    }

    private static long? ToCount(ulong value) => value <= long.MaxValue ? (long)value : null;

    private static bool? LowMemory()
    {
        using SafeWaitHandle notification = CreateMemoryResourceNotification(0);
        return !notification.IsInvalid && QueryMemoryResourceNotification(notification, out bool low)
            ? low
            : null;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct MemoryStatus
    {
        public uint Length;
        public uint Load;
        public ulong TotalPhysical;
        public ulong AvailablePhysical;
        public ulong TotalPageFile;
        public ulong AvailablePageFile;
        public ulong TotalVirtual;
        public ulong AvailableVirtual;
        public ulong AvailableExtendedVirtual;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct PowerStatus
    {
        public byte AcLineStatus;
        public byte BatteryFlag;
        public byte BatteryPercent;
        public byte Saver;
        public uint BatteryLifeTime;
        public uint BatteryFullLifeTime;
    }

    [DllImport("kernel32.dll", ExactSpelling = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GlobalMemoryStatusEx(ref MemoryStatus status);

    [DllImport("kernel32.dll", ExactSpelling = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetSystemPowerStatus(out PowerStatus status);

    [DllImport("kernel32.dll", EntryPoint = "GetDiskFreeSpaceExW", ExactSpelling = true, CharSet = CharSet.Unicode)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetDiskFreeSpaceEx(string path, out ulong available, out ulong total, out ulong free);

    [DllImport("kernel32.dll", ExactSpelling = true)]
    private static extern SafeWaitHandle CreateMemoryResourceNotification(int type);

    [DllImport("kernel32.dll", ExactSpelling = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool QueryMemoryResourceNotification(SafeWaitHandle handle,
        [MarshalAs(UnmanagedType.Bool)] out bool low);
}
