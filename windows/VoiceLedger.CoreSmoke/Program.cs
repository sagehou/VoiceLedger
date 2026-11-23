using System.Globalization;
using System.Security.Cryptography;
using VoiceLedger.Core;

if (args.Length != 2)
{
    throw new ArgumentException("Supply the ABI fixture and the generated domain fixture directory.");
}
string fixture = args[0];
var sessions = new Dictionary<string, CoreSession>();
int cases = 0;

try
{
    foreach (string line in File.ReadLines(fixture))
    {
        if (string.IsNullOrWhiteSpace(line) || line.StartsWith('#'))
        {
            continue;
        }
        string[] fields = line.Split('\t');
        int actual = Outcome(() => fields[0] switch
        {
            "session" => (int)Session(fields[1]).Apply((SessionCommand)Number(fields[2]), Long(fields[3])),
            "recommend" => (int)NativeCore.Recommend((Compatibility)Number(fields[1]), Memory(fields[2]), Memory(fields[3])),
            _ => throw new InvalidDataException($"Unknown golden fixture operation {fields[0]}.")
        });
        Expect(Number(fields[4]), actual, line);
        cases++;
    }
}
finally
{
    foreach (CoreSession session in sessions.Values)
    {
        session.Dispose();
    }
}

if (cases == 0)
{
    throw new InvalidDataException("The golden fixture contains no test cases.");
}
Expect(1, NativeCore.AbiVersion, "ABI version");
const int cycles = 128;
for (int index = 0; index < cycles; index++)
{
    using var session = new CoreSession();
    Expect(1, (int)session.Apply(SessionCommand.Prepare, 10), "prepare before cancellation");
    session.Cancel();
    session.Cancel();
    Expect(-4, Outcome(() => (int)session.Apply(SessionCommand.Start, 20)), "cancelled transition");
}

string fixtureHash = Convert.ToHexStringLower(SHA256.HashData(File.ReadAllBytes(fixture)));
Console.WriteLine($"P/Invoke ABI {NativeCore.AbiVersion} passed {cases} fixture cases and {cycles} ownership cycles.");
Console.WriteLine($"Fixture SHA-256 {fixtureHash}");
ValidateDomainFixtures(args[1]);
DeviceProfileSmoke.Run();
await RecordingSmoke.RunAsync();

static void ValidateDomainFixtures(string directory)
{
    string index = Path.Combine(directory, "cases.tsv");
    int count = 0;
    foreach (string row in File.ReadLines(index))
    {
        string[] fields = row.Split('\t');
        byte[] data = File.ReadAllBytes(Path.Combine(directory, fields[0] + ".json"));
        string hash = Convert.ToHexStringLower(SHA256.HashData(data));
        if (hash != fields[2])
        {
            throw new InvalidDataException($"Domain fixture digest mismatch for {fields[0]}.");
        }
        Expect(Number(fields[1]), Outcome(() => NativeCore.ValidateDomain(data)), fields[0]);
        count++;
    }
    if (count == 0)
    {
        throw new InvalidDataException("The domain fixture suite contains no cases.");
    }
    string suiteHash = Convert.ToHexStringLower(SHA256.HashData(File.ReadAllBytes(index)));
    Console.WriteLine($"Domain schema v1 passed {count} cases. Suite SHA-256 {suiteHash}");
}

CoreSession Session(string name)
{
    if (!sessions.TryGetValue(name, out CoreSession? session))
    {
        session = new CoreSession();
        sessions.Add(name, session);
    }
    return session;
}

static long? Memory(string value) => value == "-1" ? null : Long(value);
static int Number(string value) => int.Parse(value, CultureInfo.InvariantCulture);
static long Long(string value) => long.Parse(value, CultureInfo.InvariantCulture);

static int Outcome(Func<int> operation)
{
    try
    {
        return operation();
    }
    catch (CoreException error)
    {
        return error.Code;
    }
}

static void Expect(int expected, int actual, string context)
{
    if (expected != actual)
    {
        throw new InvalidOperationException($"{context} expected {expected}, got {actual}.");
    }
}
