using System.ComponentModel;
using System.Runtime.CompilerServices;

namespace PrivateAIGateway.Windows;

public sealed class RuntimeStore : INotifyPropertyChanged, IAsyncDisposable
{
    private readonly SemaphoreSlim lifecycleLock = new(1, 1);
    private RuntimeClient? client;
    private Task cleanupTask = Task.CompletedTask;
    private GatewayState state = GatewayState.Empty;
    private AgentStatus[] agents = [];
    private UsagePage usage = UsagePage.Empty;
    private string clientKey = "";
    private bool busy;
    private bool runtimeAvailable;
    private bool disposing;

    public event PropertyChangedEventHandler? PropertyChanged;
    public event Action<string>? Error;
    public GatewayState State { get => state; private set => Set(ref state, value); }
    public AgentStatus[] Agents { get => agents; private set => Set(ref agents, value); }
    public UsagePage Usage { get => usage; private set => Set(ref usage, value); }
    public string ClientKey { get => clientKey; private set => Set(ref clientKey, value); }
    public bool IsBusy { get => busy; private set => Set(ref busy, value); }
    public bool IsRuntimeAvailable { get => runtimeAvailable; private set => Set(ref runtimeAvailable, value); }
    public string? UsageAgent { get; set; }
    public string? UsageModel { get; set; }
    public UsageRange UsageRange { get; set; } = UsageRange.ThirtyDays;
    public bool IsRunning => IsRuntimeAvailable &&
        (State.Status == "verifying" || State.Status == "verified" || State.Status == "blocked");
    public bool IsProtected => IsRuntimeAvailable && RuntimePresentation.IsProtected(State);
    public bool IsDevMode => !State.Config.RequireProductionOs;
    public ConfidentialProfile? ActiveProfile => State.Profiles.FirstOrDefault(profile => profile.Id == State.ActiveProfileId);

    public Task<bool> InitializeAsync() => RestartAsync();

    public async Task<bool> RestartAsync()
    {
        await lifecycleLock.WaitAsync();
        try
        {
            if (disposing) return false;
            IsBusy = true;
            IsRuntimeAvailable = false;
            ClearRuntimeData();
            await cleanupTask;
            cleanupTask = Task.CompletedTask;
            if (client is { } previous)
            {
                client = null;
                await previous.DisposeAsync();
            }

            var next = new RuntimeClient();
            client = next;
            next.StateChanged += state => App.MainWindow.DispatcherQueue.TryEnqueue(() => Accept(next, state));
            next.Exited += error => App.MainWindow.DispatcherQueue.TryEnqueue(() => RuntimeExited(next, error));
            try
            {
                await next.StartAsync();
                await LoadInitialDataAsync(next);
                EnsureOwned(next);
                if (!next.IsAvailable)
                    throw new RuntimeException("runtime_exited", "The desktop runtime stopped during startup.");
                IsRuntimeAvailable = true;
                return true;
            }
            catch (Exception error)
            {
                if (ReferenceEquals(client, next)) client = null;
                IsRuntimeAvailable = false;
                ClearRuntimeData();
                await next.DisposeAsync();
                ReportError(error);
                return false;
            }
        }
        finally
        {
            IsBusy = false;
            lifecycleLock.Release();
        }
    }

    public Task<GatewayState> SetProtectionAsync(bool enabled)
    {
        var runtime = CurrentClient();
        return RunStateAsync(runtime, enabled
            ? runtime.RequestAsync<GatewayState>("start", new StartParams(State.Config))
            : runtime.RequestAsync<GatewayState>("stop", new { }));
    }

    public async Task<(bool Success, string? Error)> VerifyAndSaveAsync(ConfidentialProfileInput profile, bool allowDevOs, string? key)
    {
        try
        {
            var runtime = CurrentClient();
            IsBusy = true;
            var next = await runtime.RequestAsync<GatewayState>("verifyConfiguration", new VerifyParams(
                profile, !allowDevOs, string.IsNullOrWhiteSpace(key) ? null : key));
            AcceptCurrent(runtime, next);
            return (true, null);
        }
        catch (Exception error) { return (false, error.Message); }
        finally { IsBusy = false; }
    }

    public Task<GatewayState> ActivateProfileAsync(string id)
    {
        var runtime = CurrentClient();
        return RunStateAsync(runtime, runtime.RequestAsync<GatewayState>("activateProfile", new ProfileParams(id)));
    }

    public Task<GatewayState> DeleteProfileAsync(string id)
    {
        var runtime = CurrentClient();
        return RunStateAsync(runtime, runtime.RequestAsync<GatewayState>("deleteProfile", new ProfileParams(id)));
    }

    public Task<GatewayState> ClearCredentialAsync()
    {
        var runtime = CurrentClient();
        return RunStateAsync(runtime, runtime.RequestAsync<GatewayState>("clearApiKey", new { }));
    }

    public Task<GatewayState> SaveLocalApiAsync(LocalApiConfig config)
    {
        var runtime = CurrentClient();
        return RunStateAsync(runtime, runtime.RequestAsync<GatewayState>("saveLocalApiConfig", new LocalApiParams(config)));
    }

    public async Task SetAgentAsync(AgentStatus agent, bool connected)
    {
        try
        {
            var runtime = CurrentClient();
            var defaultModel = agent.Id == "codex" ? State.Catalog?.Models.FirstOrDefault()?.Id : null;
            var options = new ConnectOptions(defaultModel);
            var preview = await runtime.RequestAsync<AgentPreview>("previewAgent", new AgentParams(agent.Id, connected, options));
            EnsureCurrent(runtime);
            await runtime.RequestAsync<AgentStatus>("applyAgent", new ApplyAgentParams(agent.Id, connected, preview.Revision, options));
            await ReloadAgentsAsync(runtime, true);
        }
        catch (Exception error) { ReportError(error); }
    }

    public async Task RestoreAllAgentsAsync()
    {
        try
        {
            var runtime = CurrentClient();
            var result = await runtime.RequestAsync<AgentStatus[]>("disconnectAllAgents", new { });
            EnsureCurrent(runtime);
            Agents = result;
        }
        catch (Exception error) { ReportError(error); }
    }

    public Task ReloadAgentsAsync() => ReloadAgentsAsync(CurrentClient(), true);
    public Task ReloadUsageAsync(bool reset) => ReloadUsageAsync(CurrentClient(), reset, true);

    public async Task ExportUsageAsync(string path)
    {
        try
        {
            var runtime = CurrentClient();
            await runtime.RequestAsync<int>("exportUsageCsv", new ExportUsageParams(CurrentUsageQuery(null, null), path));
            EnsureCurrent(runtime);
        }
        catch (Exception error) { ReportError(error); }
    }

    public async Task ClearUsageAsync()
    {
        try
        {
            var runtime = CurrentClient();
            await runtime.RequestAsync<ulong>("clearUsage", new { });
            EnsureCurrent(runtime);
            await ReloadUsageAsync(runtime, true, true);
        }
        catch (Exception error) { ReportError(error); }
    }

    public async Task RotateClientKeyAsync()
    {
        try
        {
            var runtime = CurrentClient();
            var result = await runtime.RequestAsync<string>("rotateClientKey", new { });
            EnsureCurrent(runtime);
            ClientKey = result;
        }
        catch (Exception error) { ReportError(error); }
    }

    private async Task LoadInitialDataAsync(RuntimeClient runtime)
    {
        var stateTask = runtime.RequestAsync<GatewayState>("getState", new { });
        var agentsTask = runtime.RequestAsync<AgentStatus[]>("listAgents", new { });
        var usageTask = runtime.RequestAsync<UsagePage>("queryUsage", new UsageParams(CurrentUsageQuery(null, 20)));
        var keyTask = runtime.RequestAsync<string>("getClientKey", new { });
        await Task.WhenAll(new Task[] { stateTask, agentsTask, usageTask, keyTask });
        EnsureOwned(runtime);
        State = await stateTask;
        Agents = await agentsTask;
        Usage = await usageTask;
        ClientKey = await keyTask;
        RaiseDerivedState();
    }

    private async Task ReloadAgentsAsync(RuntimeClient runtime, bool reportError)
    {
        try
        {
            var result = await runtime.RequestAsync<AgentStatus[]>("listAgents", new { });
            EnsureCurrent(runtime);
            Agents = result;
        }
        catch (Exception error)
        {
            if (reportError) ReportError(error);
            else throw;
        }
    }

    private async Task ReloadUsageAsync(RuntimeClient runtime, bool reset, bool reportError)
    {
        try
        {
            var query = CurrentUsageQuery(reset ? null : Usage.NextCursor, 20);
            var page = await runtime.RequestAsync<UsagePage>("queryUsage", new UsageParams(query));
            EnsureCurrent(runtime);
            Usage = reset ? page : page with { Items = [.. Usage.Items, .. page.Items] };
        }
        catch (Exception error)
        {
            if (reportError) ReportError(error);
            else throw;
        }
    }

    private UsageQuery CurrentUsageQuery(string? cursor, int? limit)
    {
        var now = DateTimeOffset.UtcNow.ToUnixTimeSeconds();
        ulong? since = UsageRange switch
        {
            UsageRange.SevenDays => (ulong)Math.Max(0, now - 7 * 86_400),
            UsageRange.ThirtyDays => (ulong)Math.Max(0, now - 30 * 86_400),
            _ => null,
        };
        return new(UsageAgent, UsageModel, null, since, null, cursor, limit);
    }

    private async Task<GatewayState> RunStateAsync(RuntimeClient runtime, Task<GatewayState> operation)
    {
        try
        {
            IsBusy = true;
            var next = await operation;
            AcceptCurrent(runtime, next);
            return next;
        }
        catch (Exception error)
        {
            ReportError(error);
            throw;
        }
        finally { IsBusy = false; }
    }

    private void Accept(RuntimeClient source, GatewayState next)
    {
        if (ReferenceEquals(client, source) && source.IsAvailable && IsRuntimeAvailable) AcceptState(next);
    }

    private void AcceptCurrent(RuntimeClient source, GatewayState next)
    {
        EnsureCurrent(source);
        AcceptState(next);
    }

    private void AcceptState(GatewayState next)
    {
        var usageChanged = next.UsageRevision != State.UsageRevision;
        State = next;
        RaiseDerivedState();
        if (usageChanged && client is { } runtime && IsRuntimeAvailable) _ = ReloadUsageAsync(runtime, true, true);
    }

    private void RuntimeExited(RuntimeClient exited, Exception? error)
    {
        if (!ReferenceEquals(client, exited) || disposing) return;
        IsRuntimeAvailable = false;
        IsBusy = false;
        ClearRuntimeData();
        cleanupTask = exited.DisposeAsync().AsTask();
        Error?.Invoke(error?.Message ?? "The desktop runtime stopped.");
    }

    private void ClearRuntimeData()
    {
        State = GatewayState.Empty;
        Agents = [];
        Usage = UsagePage.Empty;
        ClientKey = "";
        RaiseDerivedState();
    }

    private RuntimeClient CurrentClient() => client is { } runtime && IsRuntimeAvailable
        ? runtime
        : throw new RuntimeException("runtime_unavailable", "The desktop runtime is not running. Restart it to continue.");

    private void EnsureCurrent(RuntimeClient source)
    {
        if (!ReferenceEquals(client, source) || !IsRuntimeAvailable)
            throw new RuntimeException("runtime_restarted", "The desktop runtime restarted before the operation completed.");
    }

    private void EnsureOwned(RuntimeClient source)
    {
        if (!ReferenceEquals(client, source))
            throw new RuntimeException("runtime_restarted", "The desktop runtime restarted before the operation completed.");
    }

    private void ReportError(Exception error)
    {
        if (!disposing) Error?.Invoke(error.Message);
    }

    private void RaiseDerivedState()
    {
        OnPropertyChanged(nameof(IsRunning));
        OnPropertyChanged(nameof(IsProtected));
        OnPropertyChanged(nameof(IsDevMode));
        OnPropertyChanged(nameof(ActiveProfile));
    }

    private void Set<T>(ref T field, T value, [CallerMemberName] string? name = null)
    {
        if (EqualityComparer<T>.Default.Equals(field, value)) return;
        field = value;
        OnPropertyChanged(name);
    }

    private void OnPropertyChanged(string? name) => PropertyChanged?.Invoke(this, new(name));

    public async ValueTask DisposeAsync()
    {
        disposing = true;
        await lifecycleLock.WaitAsync();
        try
        {
            IsRuntimeAvailable = false;
            await cleanupTask;
            if (client is { } runtime)
            {
                client = null;
                await runtime.DisposeAsync();
            }
        }
        finally { lifecycleLock.Release(); }
    }
}

public enum UsageRange { SevenDays, ThirtyDays, AllTime }
