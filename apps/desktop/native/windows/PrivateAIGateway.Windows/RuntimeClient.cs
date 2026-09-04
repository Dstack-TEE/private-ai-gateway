using System.Collections.Concurrent;
using System.Diagnostics;
using System.Text;
using System.Text.Json;

namespace PrivateAIGateway.Windows;

public sealed class RuntimeException(string code, string message) : Exception(message)
{
    public string Code { get; } = code;
}

public sealed class RuntimeClient : IAsyncDisposable
{
    private const int MaxMessageBytes = 1024 * 1024;
    private readonly JsonSerializerOptions json = new(JsonSerializerDefaults.Web);
    private readonly ConcurrentDictionary<string, TaskCompletionSource<JsonElement>> pending = new();
    private readonly SemaphoreSlim writeLock = new(1, 1);
    private Process? process;
    private StreamWriter? input;
    private long nextId;

    public event Action<GatewayState>? StateChanged;
    public event Action<Exception?>? Exited;

    public async Task StartAsync(CancellationToken cancellationToken = default)
    {
        var configured = Environment.GetEnvironmentVariable("PRIVATE_AI_GATEWAY_RUNTIME");
        var service = string.IsNullOrWhiteSpace(configured)
            ? Path.Combine(AppContext.BaseDirectory, "private-ai-gateway-desktop-service.exe")
            : configured;
        if (!File.Exists(service)) throw new RuntimeException("runtime_missing", "The bundled desktop runtime is missing.");
        process = new Process
        {
            StartInfo = new ProcessStartInfo(service)
            {
                UseShellExecute = false,
                RedirectStandardInput = true,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
                CreateNoWindow = true,
            },
            EnableRaisingEvents = true,
        };
        process.Exited += (_, _) => Exited?.Invoke(process.ExitCode == 0 ? null : new RuntimeException("runtime_exited", $"The desktop runtime exited with status {process.ExitCode}."));
        if (!process.Start()) throw new RuntimeException("runtime_start_failed", "The desktop runtime could not be started.");
        input = process.StandardInput;
        _ = Task.Run(() => ReadAsync(process.StandardOutput, cancellationToken), cancellationToken);
        _ = Task.Run(() => DrainErrorsAsync(process.StandardError, cancellationToken), cancellationToken);
        await Task.Yield();
    }

    public async Task<T> RequestAsync<T>(string method, object? parameters = null, CancellationToken cancellationToken = default)
    {
        if (input is null) throw new RuntimeException("runtime_unavailable", "The desktop runtime is not running.");
        var id = Interlocked.Increment(ref nextId).ToString();
        var completion = new TaskCompletionSource<JsonElement>(TaskCreationOptions.RunContinuationsAsynchronously);
        if (!pending.TryAdd(id, completion)) throw new RuntimeException("request_conflict", "A runtime request id was reused.");
        var payload = JsonSerializer.Serialize(new { schemaVersion = 1, id, method, @params = parameters }, json);
        if (Encoding.UTF8.GetByteCount(payload) > MaxMessageBytes)
        {
            pending.TryRemove(id, out _);
            throw new RuntimeException("message_too_large", "Desktop runtime message exceeds the 1 MiB limit.");
        }
        await writeLock.WaitAsync(cancellationToken);
        try { await input.WriteLineAsync(payload.AsMemory(), cancellationToken); await input.FlushAsync(cancellationToken); }
        catch { pending.TryRemove(id, out _); throw; }
        finally { writeLock.Release(); }
        using var registration = cancellationToken.Register(() => completion.TrySetCanceled(cancellationToken));
        var element = await completion.Task;
        if (typeof(T) == typeof(JsonElement)) return (T)(object)element;
        return element.Deserialize<T>(json) ?? throw new RuntimeException("invalid_response", "The desktop runtime returned an invalid response.");
    }

    private async Task ReadAsync(StreamReader reader, CancellationToken cancellationToken)
    {
        try
        {
            while (!cancellationToken.IsCancellationRequested && await reader.ReadLineAsync(cancellationToken) is { } line)
            {
                if (Encoding.UTF8.GetByteCount(line) > MaxMessageBytes) throw new RuntimeException("message_too_large", "Desktop runtime response exceeds the 1 MiB limit.");
                using var document = JsonDocument.Parse(line);
                var root = document.RootElement;
                if (root.TryGetProperty("event", out var eventName) && eventName.GetString() == "stateChanged")
                {
                    var state = root.GetProperty("payload").Deserialize<GatewayState>(json);
                    if (state is not null) StateChanged?.Invoke(state);
                    continue;
                }
                if (!root.TryGetProperty("id", out var idValue) || !pending.TryRemove(idValue.GetString() ?? "", out var completion)) continue;
                if (root.TryGetProperty("error", out var error))
                {
                    completion.TrySetException(new RuntimeException(
                        error.TryGetProperty("code", out var code) ? code.GetString() ?? "operation_failed" : "operation_failed",
                        error.TryGetProperty("message", out var message) ? message.GetString() ?? "The operation failed." : "The operation failed."));
                }
                else completion.TrySetResult(root.GetProperty("result").Clone());
            }
        }
        catch (Exception error) { Exited?.Invoke(error); }
    }

    private static async Task DrainErrorsAsync(StreamReader reader, CancellationToken cancellationToken)
    {
        while (!cancellationToken.IsCancellationRequested && await reader.ReadLineAsync(cancellationToken) is not null) { }
    }

    public async ValueTask DisposeAsync()
    {
        if (process is { HasExited: false })
        {
            try { await RequestAsync<JsonElement>("shutdown", new { }); }
            catch { process.Kill(true); }
        }
        process?.Dispose();
        writeLock.Dispose();
    }
}
