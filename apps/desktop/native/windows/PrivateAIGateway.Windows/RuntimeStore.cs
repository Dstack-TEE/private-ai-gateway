using System.ComponentModel;
using System.Runtime.CompilerServices;
using System.Text.Json;

namespace PrivateAIGateway.Windows;

public sealed class RuntimeStore : INotifyPropertyChanged, IAsyncDisposable
{
    private readonly RuntimeClient client = new();
    private GatewayState state = GatewayState.Empty;
    private AgentStatus[] agents = [];
    private UsagePage usage = UsagePage.Empty;
    private string clientKey = "";
    private bool busy;

    public event PropertyChangedEventHandler? PropertyChanged;
    public event Action<string>? Error;
    public GatewayState State { get => state; private set => Set(ref state, value); }
    public AgentStatus[] Agents { get => agents; private set => Set(ref agents, value); }
    public UsagePage Usage { get => usage; private set => Set(ref usage, value); }
    public string ClientKey { get => clientKey; private set => Set(ref clientKey, value); }
    public bool IsBusy { get => busy; private set => Set(ref busy, value); }
    public string? UsageAgent { get; set; }
    public string? UsageModel { get; set; }
    public UsageRange UsageRange { get; set; } = UsageRange.ThirtyDays;
    public bool IsRunning => State.Status is "verifying" or "verified" or "blocked";
    public bool IsProtected => State.Status == "verified" && !State.ConfigurationVerification;
    public bool IsDevMode => !State.Config.RequireProductionOs;
    public ConfidentialProfile? ActiveProfile => State.Profiles.FirstOrDefault(profile => profile.Id == State.ActiveProfileId);

    public async Task InitializeAsync()
    {
        client.StateChanged += next => App.MainWindow.DispatcherQueue.TryEnqueue(() => Accept(next));
        client.Exited += error => App.MainWindow.DispatcherQueue.TryEnqueue(() => Error?.Invoke(error?.Message ?? "The desktop runtime stopped."));
        await client.StartAsync();
        await Task.WhenAll(ReloadStateAsync(), ReloadAgentsAsync(), ReloadUsageAsync(true), ReloadClientKeyAsync());
    }

    public Task<GatewayState> SetProtectionAsync(bool enabled) => RunStateAsync(enabled
        ? client.RequestAsync<GatewayState>("start", new StartParams(State.Config))
        : client.RequestAsync<GatewayState>("stop", new { }));

    public async Task<bool> VerifyAndSaveAsync(ConfidentialProfileInput profile, bool allowDevOs, string? key)
    {
        try
        {
            IsBusy = true;
            Accept(await client.RequestAsync<GatewayState>("verifyConfiguration", new VerifyParams(
                profile, !allowDevOs, string.IsNullOrWhiteSpace(key) ? null : key)));
            return true;
        }
        catch (Exception error) { Error?.Invoke(error.Message); return false; }
        finally { IsBusy = false; }
    }

    public Task<GatewayState> ActivateProfileAsync(string id) => RunStateAsync(
        client.RequestAsync<GatewayState>("activateProfile", new ProfileParams(id)));
    public Task<GatewayState> DeleteProfileAsync(string id) => RunStateAsync(
        client.RequestAsync<GatewayState>("deleteProfile", new ProfileParams(id)));
    public Task<GatewayState> ClearCredentialAsync() => RunStateAsync(
        client.RequestAsync<GatewayState>("clearApiKey", new { }));
    public Task<GatewayState> SaveLocalApiAsync(LocalApiConfig config) => RunStateAsync(
        client.RequestAsync<GatewayState>("saveLocalApiConfig", new LocalApiParams(config)));

    public async Task SetAgentAsync(AgentStatus agent, bool connected)
    {
        try
        {
            var defaultModel = agent.Id == "codex" ? State.Catalog?.Models.FirstOrDefault()?.Id : null;
            var options = new ConnectOptions(defaultModel);
            var preview = await client.RequestAsync<AgentPreview>("previewAgent", new AgentParams(agent.Id, connected, options));
            await client.RequestAsync<AgentStatus>("applyAgent", new ApplyAgentParams(agent.Id, connected, preview.Revision, options));
            await ReloadAgentsAsync();
        }
        catch (Exception error) { Error?.Invoke(error.Message); }
    }

    public async Task RestoreAllAgentsAsync()
    {
        try { Agents = await client.RequestAsync<AgentStatus[]>("disconnectAllAgents", new { }); }
        catch (Exception error) { Error?.Invoke(error.Message); }
    }

    public async Task ReloadAgentsAsync()
    {
        try { Agents = await client.RequestAsync<AgentStatus[]>("listAgents", new { }); }
        catch (Exception error) { Error?.Invoke(error.Message); }
    }

    public async Task ReloadUsageAsync(bool reset)
    {
        try
        {
            var query = CurrentUsageQuery(reset ? null : Usage.NextCursor, 20);
            var page = await client.RequestAsync<UsagePage>("queryUsage", new UsageParams(query));
            Usage = reset ? page : page with { Items = [.. Usage.Items, .. page.Items] };
        }
        catch (Exception error) { Error?.Invoke(error.Message); }
    }

    public async Task ExportUsageAsync(string path)
    {
        try { await client.RequestAsync<int>("exportUsageCsv", new ExportUsageParams(CurrentUsageQuery(null, null), path)); }
        catch (Exception error) { Error?.Invoke(error.Message); }
    }

    public async Task ClearUsageAsync()
    {
        try { await client.RequestAsync<ulong>("clearUsage", new { }); await ReloadUsageAsync(true); }
        catch (Exception error) { Error?.Invoke(error.Message); }
    }

    public async Task RotateClientKeyAsync()
    {
        try { ClientKey = await client.RequestAsync<string>("rotateClientKey", new { }); }
        catch (Exception error) { Error?.Invoke(error.Message); }
    }

    private async Task ReloadStateAsync()
    {
        try { Accept(await client.RequestAsync<GatewayState>("getState", new { })); }
        catch (Exception error) { Error?.Invoke(error.Message); }
    }

    private async Task ReloadClientKeyAsync()
    {
        try { ClientKey = await client.RequestAsync<string>("getClientKey", new { }); }
        catch (Exception error) { Error?.Invoke(error.Message); }
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

    private async Task<GatewayState> RunStateAsync(Task<GatewayState> operation)
    {
        try { IsBusy = true; var next = await operation; Accept(next); return next; }
        catch (Exception error) { Error?.Invoke(error.Message); throw; }
        finally { IsBusy = false; }
    }

    private void Accept(GatewayState next)
    {
        var usageChanged = next.UsageRevision != State.UsageRevision;
        State = next;
        OnPropertyChanged(nameof(IsRunning));
        OnPropertyChanged(nameof(IsProtected));
        OnPropertyChanged(nameof(IsDevMode));
        OnPropertyChanged(nameof(ActiveProfile));
        if (usageChanged) _ = ReloadUsageAsync(true);
    }

    private void Set<T>(ref T field, T value, [CallerMemberName] string? name = null)
    {
        if (EqualityComparer<T>.Default.Equals(field, value)) return;
        field = value;
        OnPropertyChanged(name);
    }

    private void OnPropertyChanged(string? name) => PropertyChanged?.Invoke(this, new(name));
    public ValueTask DisposeAsync() => client.DisposeAsync();
}

public enum UsageRange { SevenDays, ThirtyDays, AllTime }
