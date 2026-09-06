using System.Collections.Concurrent;
using System.Diagnostics;
using System.Text;
using System.Text.Json;

namespace PrivateAIGateway.Windows;

public sealed class RuntimeException(string code, string message, Exception? innerException = null) : Exception(message, innerException)
{
    public string Code { get; } = code;
}

public sealed class RuntimeClient : IAsyncDisposable
{
    private const int MaxMessageBytes = 1024 * 1024;
    private static readonly TimeSpan RequestTimeout = TimeSpan.FromSeconds(75);
    private static readonly TimeSpan ShutdownTimeout = TimeSpan.FromSeconds(5);
    private static readonly TimeSpan DrainTimeout = TimeSpan.FromSeconds(2);
    private readonly JsonSerializerOptions json = new(JsonSerializerDefaults.Web);
    private readonly UTF8Encoding utf8 = new(false, true);
    private readonly ConcurrentDictionary<string, TaskCompletionSource<JsonElement>> pending = new();
    private readonly SemaphoreSlim writeLock = new(1, 1);
    private readonly CancellationTokenSource lifetime = new();
    private readonly object disposeLock = new();
    private Process? process;
    private StreamWriter? input;
    private Task? readTask;
    private Task? errorTask;
    private Exception? failure;
    private Task? disposeTask;
    private long nextId;
    private int failed;
    private int disposing;

    public event Action<GatewayState>? StateChanged;
    public event Action<Exception?>? Exited;
    internal bool IsAvailable => Volatile.Read(ref failed) == 0 && IsRunning(process);

    public async Task StartAsync(CancellationToken cancellationToken = default)
    {
        cancellationToken.ThrowIfCancellationRequested();
        var configured = Environment.GetEnvironmentVariable("PRIVATE_AI_GATEWAY_RUNTIME");
        var service = string.IsNullOrWhiteSpace(configured)
            ? Path.Combine(AppContext.BaseDirectory, "private-ai-gateway-desktop-service.exe")
            : configured;
        if (!File.Exists(service)) throw new RuntimeException("runtime_missing", "The bundled desktop runtime is missing.");

        var started = new Process
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
        started.Exited += ProcessExited;
        process = started;
        try
        {
            if (!started.Start()) throw new RuntimeException("runtime_start_failed", "The desktop runtime could not be started.");
        }
        catch (Exception error)
        {
            started.Exited -= ProcessExited;
            process = null;
            started.Dispose();
            throw error is RuntimeException ? error : new RuntimeException("runtime_start_failed", "The desktop runtime could not be started.", error);
        }

        input = started.StandardInput;
        readTask = Task.Run(() => ReadAsync(started.StandardOutput.BaseStream, lifetime.Token));
        errorTask = Task.Run(() => DrainErrorsAsync(started.StandardError, lifetime.Token));
        await Task.Yield();
        if (Volatile.Read(ref failed) != 0)
            throw failure ?? new RuntimeException("runtime_exited", "The desktop runtime stopped during startup.");
    }

    public async Task<T> RequestAsync<T>(string method, object? parameters = null, CancellationToken cancellationToken = default)
    {
        var id = Interlocked.Increment(ref nextId).ToString();
        var payload = JsonSerializer.Serialize(new { schemaVersion = 1, id, method, @params = parameters }, json);
        if (Encoding.UTF8.GetByteCount(payload) > MaxMessageBytes)
            throw new RuntimeException("message_too_large", "Desktop runtime message exceeds the 1 MiB limit.");

        var writer = input ?? throw new RuntimeException("runtime_unavailable", "The desktop runtime is not running.");
        var completion = new TaskCompletionSource<JsonElement>(TaskCreationOptions.RunContinuationsAsynchronously);
        if (!pending.TryAdd(id, completion)) throw new RuntimeException("request_conflict", "A runtime request id was reused.");
        using var timeout = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken, lifetime.Token);
        timeout.CancelAfter(RequestTimeout);
        using var registration = timeout.Token.Register(() => completion.TrySetCanceled(timeout.Token));
        try
        {
            if (Volatile.Read(ref failed) != 0)
                throw failure ?? new RuntimeException("runtime_unavailable", "The desktop runtime is not running.");
            await writeLock.WaitAsync(timeout.Token);
            try
            {
                await writer.WriteLineAsync(payload.AsMemory(), timeout.Token);
                await writer.FlushAsync(timeout.Token);
            }
            finally { writeLock.Release(); }
            var element = await completion.Task;
            if (typeof(T) == typeof(JsonElement)) return (T)(object)element;
            return element.Deserialize<T>(json) ?? throw new RuntimeException("invalid_response", "The desktop runtime returned an invalid response.");
        }
        catch (OperationCanceledException) when (lifetime.IsCancellationRequested && !cancellationToken.IsCancellationRequested)
        {
            throw failure ?? new RuntimeException("runtime_unavailable", "The desktop runtime is not running.");
        }
        catch (OperationCanceledException) when (!cancellationToken.IsCancellationRequested)
        {
            var timeoutError = new RuntimeException("request_timeout", $"The desktop runtime did not answer {method} within {RequestTimeout.TotalSeconds:0} seconds.");
            Fail(timeoutError);
            throw timeoutError;
        }
        catch (Exception error) when (error is IOException or ObjectDisposedException)
        {
            var transport = new RuntimeException("runtime_write_failed", "The desktop runtime connection failed while sending a request.", error);
            Fail(transport);
            throw transport;
        }
        finally { pending.TryRemove(id, out _); }
    }

    private async Task ReadAsync(Stream stream, CancellationToken cancellationToken)
    {
        try
        {
            var chunk = new byte[8192];
            using var frame = new MemoryStream();
            while (!cancellationToken.IsCancellationRequested)
            {
                var count = await stream.ReadAsync(chunk, cancellationToken);
                if (count == 0)
                {
                    if (frame.Length != 0) throw new RuntimeException("invalid_response", "The desktop runtime closed its connection mid-message.");
                    Fail(new RuntimeException("runtime_eof", "The desktop runtime closed its connection."));
                    return;
                }
                var start = 0;
                for (var index = 0; index < count; index++)
                {
                    if (chunk[index] != (byte)'\n') continue;
                    AppendFrame(frame, chunk.AsSpan(start, index - start));
                    if (frame.Length > 0) HandleFrame(frame.ToArray());
                    frame.SetLength(0);
                    start = index + 1;
                }
                AppendFrame(frame, chunk.AsSpan(start, count - start));
            }
        }
        catch (OperationCanceledException) when (cancellationToken.IsCancellationRequested) { }
        catch (Exception error)
        {
            Fail(error is RuntimeException ? error : new RuntimeException("runtime_read_failed", "The desktop runtime connection failed while reading a response.", error));
        }
    }

    private void AppendFrame(MemoryStream frame, ReadOnlySpan<byte> bytes)
    {
        if (frame.Length + bytes.Length > MaxMessageBytes)
            throw new RuntimeException("message_too_large", "Desktop runtime response exceeds the 1 MiB limit.");
        frame.Write(bytes);
    }

    private void HandleFrame(byte[] frame)
    {
        try
        {
            using var document = JsonDocument.Parse(utf8.GetString(frame));
            var root = document.RootElement;
            if (root.ValueKind != JsonValueKind.Object)
                throw new RuntimeException("invalid_response", "The desktop runtime returned an invalid protocol envelope.");
            if (!root.TryGetProperty("schemaVersion", out var schema) ||
                schema.ValueKind != JsonValueKind.Number ||
                !schema.TryGetInt32(out var version) ||
                version != 1)
                throw new RuntimeException("unsupported_schema", "The desktop runtime returned an unsupported protocol version.");
            if (root.TryGetProperty("event", out var eventName))
            {
                if (eventName.ValueKind != JsonValueKind.String ||
                    eventName.GetString() != "stateChanged" ||
                    !root.TryGetProperty("payload", out var payload) ||
                    root.TryGetProperty("id", out _) ||
                    root.TryGetProperty("result", out _) ||
                    root.TryGetProperty("error", out _))
                    throw new RuntimeException("invalid_response", "The desktop runtime returned an invalid event envelope.");
                var state = payload.Deserialize<GatewayState>(json) ??
                    throw new RuntimeException("invalid_response", "The desktop runtime returned an invalid state event.");
                StateChanged?.Invoke(state);
                return;
            }
            if (!root.TryGetProperty("id", out var idValue) || idValue.ValueKind != JsonValueKind.String)
                throw new RuntimeException("invalid_response", "The desktop runtime returned a response without an id.");
            var hasResult = root.TryGetProperty("result", out var result);
            var hasError = root.TryGetProperty("error", out var error);
            if (hasResult == hasError || root.TryGetProperty("payload", out _))
                throw new RuntimeException("invalid_response", "The desktop runtime returned an ambiguous response envelope.");
            var errorCode = "operation_failed";
            var errorMessage = "The operation failed.";
            if (hasError)
            {
                if (error.ValueKind != JsonValueKind.Object ||
                    !error.TryGetProperty("code", out var code) || code.ValueKind != JsonValueKind.String ||
                    !error.TryGetProperty("message", out var message) || message.ValueKind != JsonValueKind.String)
                    throw new RuntimeException("invalid_response", "The desktop runtime returned an invalid error response.");
                errorCode = code.GetString() ?? errorCode;
                errorMessage = message.GetString() ?? errorMessage;
            }
            if (!pending.TryRemove(idValue.GetString() ?? "", out var completion)) return;
            if (hasError)
            {
                completion.TrySetException(new RuntimeException(errorCode, errorMessage));
            }
            else completion.TrySetResult(result.Clone());
        }
        catch (Exception error)
        {
            throw error is RuntimeException ? error : new RuntimeException("invalid_response", "The desktop runtime returned invalid JSON.", error);
        }
    }

    private static async Task DrainErrorsAsync(StreamReader reader, CancellationToken cancellationToken)
    {
        try
        {
            while (!cancellationToken.IsCancellationRequested && await reader.ReadLineAsync(cancellationToken) is not null) { }
        }
        catch (OperationCanceledException) when (cancellationToken.IsCancellationRequested) { }
        catch (IOException) { }
    }

    private void ProcessExited(object? sender, EventArgs args)
    {
        var exited = sender as Process;
        var status = exited is null ? null : SafeExitCode(exited);
        var error = status == 0
            ? new RuntimeException("runtime_exited", "The desktop runtime stopped.")
            : new RuntimeException("runtime_exited", status is null
                ? "The desktop runtime stopped unexpectedly."
                : $"The desktop runtime exited with status {status}.");
        Fail(error);
    }

    private static int? SafeExitCode(Process process)
    {
        try { return process.ExitCode; }
        catch (InvalidOperationException) { return null; }
    }

    private void Fail(Exception error)
    {
        if (Interlocked.Exchange(ref failed, 1) != 0) return;
        failure = error;
        foreach (var entry in pending.ToArray())
            if (pending.TryRemove(entry.Key, out var completion)) completion.TrySetException(error);
        lifetime.Cancel();
        if (Volatile.Read(ref disposing) == 0) Exited?.Invoke(error);
    }

    public ValueTask DisposeAsync()
    {
        lock (disposeLock) return new ValueTask(disposeTask ??= DisposeCoreAsync());
    }

    private async Task DisposeCoreAsync()
    {
        Interlocked.Exchange(ref disposing, 1);
        var ownedProcess = process;
        if (ownedProcess is not null && IsRunning(ownedProcess))
        {
            using var shutdown = new CancellationTokenSource(ShutdownTimeout);
            try { await RequestAsync<JsonElement>("shutdown", new { }, shutdown.Token); }
            catch (Exception) { }
            try { await ownedProcess.WaitForExitAsync(shutdown.Token); }
            catch (OperationCanceledException)
            {
                try { ownedProcess.Kill(true); }
                catch (Exception) { }
                using var killed = new CancellationTokenSource(DrainTimeout);
                try { await ownedProcess.WaitForExitAsync(killed.Token); }
                catch (Exception) { }
            }
            catch (InvalidOperationException) { }
        }

        lifetime.Cancel();
        Fail(new RuntimeException("runtime_unavailable", "The desktop runtime is not running."));
        var drains = new[] { readTask, errorTask }.Where(task => task is not null).Cast<Task>().ToArray();
        if (drains.Length > 0) await Task.WhenAny(Task.WhenAll(drains), Task.Delay(DrainTimeout));
        input?.Dispose();
        if (ownedProcess is not null) ownedProcess.Exited -= ProcessExited;
        ownedProcess?.Dispose();
    }

    private static bool IsRunning(Process? process)
    {
        if (process is null) return false;
        try { return !process.HasExited; }
        catch (InvalidOperationException) { return false; }
    }
}
