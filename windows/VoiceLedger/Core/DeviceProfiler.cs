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
            var resources = ReadResources(directory);
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
                total_memory_bytes = resources.TotalMemory,
                available_memory_bytes = resources.AvailableMemory,
                process_memory_bytes = privateBytes > 0 ? (long?)privateBytes : null,
                low_memory = resources.LowMemory,
                storage_available_bytes = resources.Storage,
                power_saver = resources.PowerSaver,
                battery_percent = resources.BatteryPercent,
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

    internal static byte[] SpeechResources(string directory)
    {
        var resources = ReadResources(directory);
        return JsonSerializer.SerializeToUtf8Bytes(new
        {
            available_memory_bytes = resources.AvailableMemory,
            low_memory = resources.LowMemory,
            storage_available_bytes = resources.Storage,
            power_saver = resources.PowerSaver,
            battery_percent = resources.BatteryPercent,
        });
    }

    private static Resources ReadResources(string directory)
    {
        var memory = new MemoryStatus { Length = (uint)Marshal.SizeOf<MemoryStatus>() };
        bool memoryKnown = GlobalMemoryStatusEx(ref memory) && memory.TotalPhysical != 0;
        bool powerKnown = GetSystemPowerStatus(out PowerStatus power);
        bool hasBattery = powerKnown && (power.BatteryFlag & 128) == 0;
        return new Resources(
            memoryKnown ? ToCount(memory.TotalPhysical) : null,
            memoryKnown ? ToCount(memory.AvailablePhysical) : null,
            LowMemory(),
            GetDiskFreeSpaceEx(directory, out ulong available, out _, out _) ? ToCount(available) : null,
            powerKnown && power.Saver <= 1 ? power.Saver == 1 : null,
            hasBattery && power.BatteryPercent <= 100 ? power.BatteryPercent : null);
    }

    private readonly record struct Resources(long? TotalMemory, long? AvailableMemory, bool? LowMemory,
        long? Storage, bool? PowerSaver, byte? BatteryPercent);

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
