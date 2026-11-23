using System.Text.Json;
using VoiceLedger.Core;

internal static class DeviceProfileSmoke
{
    internal static void Run()
    {
        string directory = Directory.CreateTempSubdirectory("voiceledger-profile-").FullName;
        try
        {
            string path = Path.Combine(directory, "device-profile-v1.json");
            Require(DeviceProfiler.Capture(directory), "Windows profile capture");
            using var first = JsonDocument.Parse(File.ReadAllBytes(path));
            var profile = first.RootElement;
            Require(profile.GetProperty("platform").GetString() == "windows", "Windows platform");
            Require(profile.GetProperty("logical_cpu_count").GetInt32() > 0, "available CPU count");
            Require(profile.GetProperty("total_memory_bytes").GetInt64() > 0, "physical memory");
            Require(profile.GetProperty("available_memory_bytes").GetInt64() >= 0, "available physical memory");
            Require(profile.GetProperty("process_memory_bytes").GetInt64() > 0, "private bytes");
            Require(profile.GetProperty("storage_available_bytes").GetInt64() >= 0, "storage available to app");
            Require(profile.GetProperty("thermal_state").ValueKind == JsonValueKind.Null, "unknown Windows thermal state");

            Require(DeviceProfiler.Capture(directory), "Windows profile refresh");
            using var second = JsonDocument.Parse(File.ReadAllBytes(path));
            Require(second.RootElement.GetProperty("device_id").GetGuid() == profile.GetProperty("device_id").GetGuid(),
                "stable local identity");
            Require(second.RootElement.GetProperty("id").GetGuid() != profile.GetProperty("id").GetGuid(),
                "new observation identity");
            Console.WriteLine("Windows device profiler passed native queries and local profile refresh on the hosted runner.");
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    private static void Require(bool condition, string check)
    {
        if (!condition) throw new InvalidDataException($"Device profile check failed for {check}.");
    }
}
