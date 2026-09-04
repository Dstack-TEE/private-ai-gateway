using Microsoft.UI;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using WinRT.Interop;

namespace PrivateAIGateway.Windows;

public sealed partial class MainWindow : Window
{
    private readonly RuntimeStore store = new();
    private readonly NativeTray tray;
    private string page = "overview";
    private bool syncingSwitch;
    private bool initialized;
    private AppWindow appWindow = null!;

    public MainWindow()
    {
        InitializeComponent();
        BrandIconHost.Children.Add(new Image
        {
            Source = new SvgImageSource(new Uri("ms-appx:///Assets/brand/mark.svg")),
            Width = 34,
            Height = 34,
        });
        var hwnd = WindowNative.GetWindowHandle(this);
        appWindow = AppWindow.GetFromWindowId(Win32Interop.GetWindowIdFromWindow(hwnd));
        appWindow.Resize(new global::Windows.Graphics.SizeInt32(1052, 820));
        appWindow.SetIcon(Path.Combine(AppContext.BaseDirectory, "Assets", "AppIcon.ico"));
        appWindow.Closing += (_, args) => { args.Cancel = true; appWindow.Hide(); };
        tray = new NativeTray(hwnd, ShowMainWindow, ShowSettings, ToggleFromTray, QuitAsync);
        store.PropertyChanged += (_, _) => DispatcherQueue.TryEnqueue(Render);
        store.Error += message => DispatcherQueue.TryEnqueue(async () => await ShowErrorAsync(message));
        Activated += async (_, _) => { if (!initialized) { initialized = true; await InitializeAsync(); } };
        Navigation.SelectedItem = Navigation.MenuItems[0];
    }

    public void ShowMainWindow()
    {
        appWindow.Show();
        Activate();
    }

    public void HideMainWindow() => appWindow.Hide();

    public void ShowSettings()
    {
        ShowMainWindow();
        Navigation.SelectedItem = Navigation.SettingsItem;
    }

    private async Task InitializeAsync()
    {
        try { await store.InitializeAsync(); Render(); }
        catch (Exception error) { await ShowErrorAsync(error.Message); }
    }

    private void Render()
    {
        syncingSwitch = true;
        ProtectionSwitch.IsOn = store.IsRunning;
        ProtectionSwitch.IsEnabled = !store.IsBusy;
        syncingSwitch = false;
        StatusText.Text = StatusLabel(store.State);
        DevBadge.Visibility = store.IsRunning && store.IsDevMode ? Visibility.Visible : Visibility.Collapsed;
        tray.Update(store.IsRunning, store.IsProtected, StatusLabel(store.State));
        PageHost.Content = page switch
        {
            "agents" => NativeViews.Agents(store),
            "usage" => NativeViews.Usage(store, this),
            "settings" => NativeViews.Settings(store, this),
            _ => NativeViews.Overview(store, this),
        };
    }

    private void Navigation_SelectionChanged(NavigationView sender, NavigationViewSelectionChangedEventArgs args)
    {
        page = args.IsSettingsSelected ? "settings" : (args.SelectedItemContainer?.Tag?.ToString() ?? "overview");
        PageTitle.Text = page switch { "agents" => "Agents", "usage" => "Usage", "settings" => "Settings", _ => "Overview" };
        Render();
    }

    private async void ProtectionSwitch_Toggled(object sender, RoutedEventArgs e)
    {
        if (syncingSwitch) return;
        if (ProtectionSwitch.IsOn && (!store.State.ApiKeySaved || store.State.Profiles.Length == 0))
        {
            syncingSwitch = true;
            ProtectionSwitch.IsOn = false;
            syncingSwitch = false;
            await ShowProfilesAsync();
            return;
        }
        try { await store.SetProtectionAsync(ProtectionSwitch.IsOn); }
        catch { Render(); }
    }

    private async void ToggleFromTray()
    {
        if (!store.IsRunning && (!store.State.ApiKeySaved || store.State.Profiles.Length == 0))
        {
            ShowMainWindow();
            await ShowProfilesAsync();
            return;
        }
        try { await store.SetProtectionAsync(!store.IsRunning); } catch { }
    }

    public async Task ShowProfilesAsync() => await NativeDialogs.ShowProfilesAsync(store, ContentRoot());
    public async Task ShowLocalApiAsync() => await NativeDialogs.ShowLocalApiAsync(store, ContentRoot());
    public async Task ShowProofAsync(RequestActivity item) => await NativeDialogs.ShowProofAsync(store.State, item, ContentRoot());
    public async Task ShowPrivacyAsync() => await NativeDialogs.ShowPrivacyAsync(store.State, ContentRoot());
    public async Task ConfirmRestoreAsync()
    {
        if (await NativeDialogs.ConfirmAsync("Restore all agent configurations?", "Private AI Gateway will revoke every managed agent token and restore its previous configuration where possible.", "Restore All", ContentRoot()))
            await store.RestoreAllAgentsAsync();
    }
    public async Task ConfirmClearUsageAsync()
    {
        if (await NativeDialogs.ConfirmAsync("Clear all usage history?", "This permanently deletes the local usage database. It does not affect provider records.", "Clear History", ContentRoot()))
            await store.ClearUsageAsync();
    }

    private async Task ShowErrorAsync(string message)
    {
        var dialog = new ContentDialog { Title = "Private AI Gateway", Content = message, CloseButtonText = "OK", XamlRoot = ContentRoot() };
        await dialog.ShowAsync();
    }

    private XamlRoot ContentRoot() => Content.XamlRoot;
    private static string StatusLabel(GatewayState state) => state.Status switch
    {
        "verifying" => state.ConfigurationVerification ? "Verifying configuration" : "Starting",
        "verified" when !state.Config.RequireProductionOs => "Protected · Dev mode",
        "verified" => "Protected",
        "blocked" => "Blocked",
        "error" => "Needs attention",
        _ => "Not protected",
    };

    private async void QuitAsync()
    {
        tray.Dispose();
        await store.DisposeAsync();
        appWindow.Destroy();
        Application.Current.Exit();
    }
}
