using System.Diagnostics;
using System.Text;
using PrivateAIGateway.Windows;

const string ChildMode = "PRIVATE_AI_GATEWAY_RUNTIME_TEST_CHILD";

if (Environment.GetEnvironmentVariable(ChildMode) is { } mode)
{
    RunChild(mode);
    return;
}

var tests = new (string Name, Func<Task> Run)[]
{
    ("pending request fails on EOF", PendingRequestFailsOnEofAsync),
    ("malformed frame disconnects", () => InvalidFrameDisconnectsAsync("malformed")),
    ("oversized frame disconnects", () => InvalidFrameDisconnectsAsync("oversized")),
    ("unresponsive shutdown is bounded", UnresponsiveShutdownIsBoundedAsync),
    ("status presentation distinguishes protection", StatusPresentationIsAccurateAsync),
};

foreach (var test in tests)
{
    await test.Run();
    Console.WriteLine($"PASS {test.Name}");
}

static async Task PendingRequestFailsOnEofAsync()
{
    await using var client = await StartClientAsync("eof-on-request");
    var exited = new TaskCompletionSource<Exception?>(TaskCreationOptions.RunContinuationsAsynchronously);
    client.Exited += error => exited.TrySetResult(error);
    var error = await ExpectRuntimeFailureAsync(client.RequestAsync<GatewayState>("getState", new { }));
    Assert(error.Code is "runtime_eof" or "runtime_exited", $"Unexpected EOF error code: {error.Code}");
    Assert(await exited.Task.WaitAsync(TimeSpan.FromSeconds(5)) is not null, "Runtime exit was not reported");
}

static async Task InvalidFrameDisconnectsAsync(string mode)
{
    await using var client = await StartClientAsync(mode);
    var error = await ExpectRuntimeFailureAsync(client.RequestAsync<GatewayState>("getState", new { }));
    var expected = mode == "oversized" ? "message_too_large" : "invalid_response";
    Assert(error.Code == expected, $"Expected {expected}, got {error.Code}");
    Assert(!client.IsAvailable, "Invalid runtime output did not disconnect the client");
}

static async Task UnresponsiveShutdownIsBoundedAsync()
{
    var client = await StartClientAsync("ignore-shutdown");
    var elapsed = Stopwatch.StartNew();
    await client.DisposeAsync();
    elapsed.Stop();
    Assert(elapsed.Elapsed < TimeSpan.FromSeconds(10), $"Shutdown took {elapsed.Elapsed.TotalSeconds:F1} seconds");
}

static Task StatusPresentationIsAccurateAsync()
{
    var development = GatewayState.Empty with
    {
        Status = "verified",
        ConfigurationVerification = false,
        Config = new StartGatewayConfig("https://example.test", false),
    };
    Assert(RuntimePresentation.IsProtected(development), "Verified runtime should be protected");
    Assert(RuntimePresentation.ShowDevMode(development), "Verified development runtime should show dev mode");
    Assert(RuntimePresentation.SummaryLabel(development) == "Protected in dev mode", "Development summary is incorrect");

    var configuration = development with { ConfigurationVerification = true };
    Assert(!RuntimePresentation.IsProtected(configuration), "Configuration verification must not be protected");
    Assert(!RuntimePresentation.ShowDevMode(configuration), "Configuration verification must not show protected dev mode");
    Assert(RuntimePresentation.StatusLabel(configuration) == "Configuration verified", "Configuration status is incorrect");

    var blocked = development with { Status = "blocked", ConfigurationVerification = false };
    Assert(!RuntimePresentation.IsProtected(blocked), "Blocked runtime must not be protected");
    Assert(!RuntimePresentation.ShowDevMode(blocked), "Blocked runtime must not show protected dev mode");
    Assert(RuntimePresentation.SummaryLabel(blocked) == "Blocked", "Blocked summary is incorrect");
    return Task.CompletedTask;
}

static async Task<RuntimeClient> StartClientAsync(string mode)
{
    var executable = Environment.ProcessPath ?? throw new InvalidOperationException("Cannot locate the test executable");
    Environment.SetEnvironmentVariable("PRIVATE_AI_GATEWAY_RUNTIME", executable);
    Environment.SetEnvironmentVariable(ChildMode, mode);
    var client = new RuntimeClient();
    try
    {
        await client.StartAsync();
        return client;
    }
    finally
    {
        Environment.SetEnvironmentVariable(ChildMode, null);
        Environment.SetEnvironmentVariable("PRIVATE_AI_GATEWAY_RUNTIME", null);
    }
}

static async Task<RuntimeException> ExpectRuntimeFailureAsync(Task operation)
{
    try
    {
        await operation.WaitAsync(TimeSpan.FromSeconds(5));
        throw new InvalidOperationException("Runtime request unexpectedly succeeded");
    }
    catch (RuntimeException error) { return error; }
}

static void RunChild(string mode)
{
    Console.OutputEncoding = new UTF8Encoding(false);
    switch (mode)
    {
        case "eof-on-request":
            Console.ReadLine();
            return;
        case "malformed":
            Console.ReadLine();
            Console.WriteLine("{");
            Console.Out.Flush();
            Thread.Sleep(TimeSpan.FromSeconds(30));
            return;
        case "oversized":
            Console.ReadLine();
            Console.Write(new string('x', 1024 * 1024 + 1));
            Console.Out.Flush();
            Thread.Sleep(TimeSpan.FromSeconds(30));
            return;
        case "ignore-shutdown":
            while (Console.ReadLine() is not null) { }
            return;
        default:
            throw new InvalidOperationException($"Unknown child mode: {mode}");
    }
}

static void Assert(bool condition, string message)
{
    if (!condition) throw new InvalidOperationException(message);
}
