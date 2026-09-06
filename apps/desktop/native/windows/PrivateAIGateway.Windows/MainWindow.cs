using System.Text.Json;
using Microsoft.UI;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using WinRT.Interop;

namespace PrivateAIGateway.Windows;

public sealed class MainWindow : Window
{
    private readonly RuntimeStore store = new();
    private readonly NativeTray tray;
    private readonly Dictionary<string, INativePage> pages = [];
    private readonly NavigationView navigation = new();
    private readonly TextBlock pageTitle = new() { Text = "Overview", FontSize = 20, FontWeight = global::Microsoft.UI.Text.FontWeights.SemiBold, VerticalAlignment = VerticalAlignment.Center };
    private readonly Border devBadge = new() { Background = new SolidColorBrush(ColorHelper.FromArgb(0x33, 0xE9, 0xA4, 0)), CornerRadius = new CornerRadius(4), Padding = new Thickness(7, 3, 7, 3), Visibility = Visibility.Collapsed };
    private readonly TextBlock statusText = new() { VerticalAlignment = VerticalAlignment.Center, Opacity = 0.7 };
    private readonly ToggleSwitch protectionSwitch = new();
    private readonly ContentPresenter pageHost = new();
    private readonly InfoBar errorBar = new() { Severity = InfoBarSeverity.Error, IsOpen = false, IsClosable = true };
    private readonly Button restartButton = new() { Content = "Restart Runtime", Visibility = Visibility.Collapsed };
    private readonly TaskCompletionSource<bool> navigationInitialized = new(TaskCreationOptions.RunContinuationsAsynchronously);
    private string page = "overview";
    private bool syncingSwitch;
    private bool initialized;
    private bool navigationReady;
    private bool quitting;
    private AppWindow appWindow = null!;

    public MainWindow()
    {
        App.Trace("window:constructing");
        Title = "Private AI Gateway";
        BuildShell();
        App.Trace("window:shell-built");
        var hwnd = WindowNative.GetWindowHandle(this);
        appWindow = AppWindow.GetFromWindowId(Win32Interop.GetWindowIdFromWindow(hwnd));
        appWindow.Resize(new global::Windows.Graphics.SizeInt32(1052, 820));
        appWindow.SetIcon(Path.Combine(AppContext.BaseDirectory, "Assets", "AppIcon.ico"));
        appWindow.Closing += (_, args) =>
        {
            args.Cancel = !quitting;
            if (!quitting) appWindow.Hide();
        };
        App.Trace("window:app-window-ready");
        tray = new NativeTray(hwnd, ShowMainWindow, ShowSettings, ToggleFromTray, QuitAsync);
        App.Trace("window:tray-ready");
        store.PropertyChanged += (_, args) => DispatcherQueue.TryEnqueue(() => Update(args.PropertyName));
        store.Error += message => DispatcherQueue.TryEnqueue(() => ShowError(message));
        restartButton.Click += async (_, _) => await RestartRuntimeAsync();
        errorBar.ActionButton = restartButton;
        Activated += async (_, _) =>
        {
            if (!initialized)
            {
                initialized = true;
                await InitializeAsync();
            }
        };
        navigation.Loaded += SelectInitialPage;
        App.Trace("window:constructed");
    }

    private void SelectInitialPage(object sender, RoutedEventArgs args)
    {
        navigation.Loaded -= SelectInitialPage;
        App.Trace("navigation:loaded");
        DispatcherQueue.TryEnqueue(() =>
        {
            navigationReady = true;
            App.Trace("navigation:selecting");
            navigation.SelectedItem = navigation.MenuItems[0];
            if (pageHost.Content is null) ShowPage("overview");
            App.Trace("navigation:selected");
            navigationInitialized.TrySetResult(true);
        });
    }

    private void BuildShell()
    {
        navigation.IsBackButtonVisible = NavigationViewBackButtonVisible.Collapsed;
        navigation.IsSettingsVisible = true;
        navigation.PaneDisplayMode = NavigationViewPaneDisplayMode.Left;
        navigation.OpenPaneLength = 220;
        navigation.SelectionChanged += Navigation_SelectionChanged;
        navigation.MenuItems.Add(NavigationItem("Overview", "overview", new SymbolIcon(Symbol.Home)));
        navigation.MenuItems.Add(NavigationItem("Agents", "agents", new FontIcon { Glyph = "\uE756" }));
        navigation.MenuItems.Add(NavigationItem("Usage", "usage", new FontIcon { Glyph = "\uE9D2" }));

        var brand = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 10, Padding = new Thickness(4, 8, 0, 16) };
        brand.Children.Add(new Image { Source = new SvgImageSource(new Uri("ms-appx:///Assets/brand/mark.svg")), Width = 34, Height = 34 });
        var brandText = new StackPanel { VerticalAlignment = VerticalAlignment.Center };
        brandText.Children.Add(new TextBlock { Text = "Private AI Gateway", FontWeight = global::Microsoft.UI.Text.FontWeights.SemiBold });
        brandText.Children.Add(new TextBlock { Text = "Confidential inference", FontSize = 12, Opacity = 0.65 });
        brand.Children.Add(brandText);
        navigation.PaneHeader = brand;

        var shell = new Grid();
        shell.RowDefinitions.Add(new RowDefinition { Height = new GridLength(58) });
        shell.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        shell.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        var header = new Grid { Padding = new Thickness(24, 0, 24, 0) };
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        header.Children.Add(pageTitle);
        devBadge.Child = new TextBlock { Text = "Dev mode", Foreground = new SolidColorBrush(ColorHelper.FromArgb(0xFF, 0xB8, 0x78, 0)), FontWeight = global::Microsoft.UI.Text.FontWeights.SemiBold };
        var controls = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 12, VerticalAlignment = VerticalAlignment.Center };
        controls.Children.Add(devBadge);
        controls.Children.Add(statusText);
        controls.Children.Add(new TextBlock { Text = "Protected", VerticalAlignment = VerticalAlignment.Center });
        protectionSwitch.Toggled += ProtectionSwitch_Toggled;
        controls.Children.Add(protectionSwitch);
        Grid.SetColumn(controls, 1);
        header.Children.Add(controls);
        shell.Children.Add(new Border
        {
            BorderBrush = new SolidColorBrush(ColorHelper.FromArgb(0x20, 0x80, 0x80, 0x80)),
            BorderThickness = new Thickness(0, 0, 0, 1),
            Child = header,
        });
        errorBar.Margin = new Thickness(24, 8, 24, 0);
        Grid.SetRow(errorBar, 1);
        shell.Children.Add(errorBar);
        Grid.SetRow(pageHost, 2);
        shell.Children.Add(pageHost);
        navigation.Content = shell;
        Content = navigation;
    }

    private static NavigationViewItem NavigationItem(string title, string tag, IconElement icon) => new() { Content = title, Tag = tag, Icon = icon };

    public void ShowMainWindow()
    {
        appWindow.Show();
        Activate();
    }

    public void HideAfterLaunch() => DispatcherQueue.TryEnqueue(() => appWindow.Hide());

    public void ShowSettings()
    {
        ShowMainWindow();
        navigation.SelectedItem = navigation.SettingsItem;
    }

    private async Task InitializeAsync()
    {
        App.Trace("runtime:initializing");
        var healthy = false;
        try
        {
            healthy = await store.InitializeAsync();
            App.Trace(healthy ? "runtime:initialized" : "runtime:unavailable");
            if (healthy) ClearError();
        }
        catch (Exception error)
        {
            App.Trace($"runtime:initialization-failed:{error.GetType().Name}");
            ShowError(error.Message);
        }
        App.Trace("runtime:updating-ui");
        Update(null);
        App.Trace("runtime:ui-updated");
        var navigationSucceeded = false;
        var pageLoaded = false;
        if (App.IsSmokeTest)
            (navigationSucceeded, pageLoaded) = await VerifySmokeUiAsync();
        var passed = healthy && store.IsRuntimeAvailable && store.Agents.Length == 5 &&
            (!App.IsSmokeTest || navigationSucceeded && pageLoaded);
        WriteHealthResult(passed, navigationSucceeded, pageLoaded);
        App.Trace(passed ? "health:passed" : "health:failed");
        if (App.IsSmokeTest)
        {
            if (!passed) Environment.ExitCode = 1;
            App.Trace("smoke:quitting");
            await QuitCoreAsync();
        }
    }

    private void Update(string? propertyName)
    {
        App.Trace($"update:{propertyName ?? "all"}:starting");
        syncingSwitch = true;
        protectionSwitch.IsOn = store.IsRunning;
        protectionSwitch.IsEnabled = store.IsRuntimeAvailable && !store.IsBusy;
        syncingSwitch = false;
        statusText.Text = store.IsRuntimeAvailable ? RuntimePresentation.StatusLabel(store.State) : "Runtime unavailable";
        devBadge.Visibility = RuntimePresentation.ShowDevMode(store.State) && store.IsRuntimeAvailable ? Visibility.Visible : Visibility.Collapsed;
        tray.Update(store.IsProtected, statusText.Text);

        if (pages.Count == 0)
        {
            App.Trace($"update:{propertyName ?? "all"}:shell-only");
            return;
        }

        switch (propertyName)
        {
            case nameof(RuntimeStore.Agents):
                UpdatePage("overview");
                UpdatePage("agents");
                break;
            case nameof(RuntimeStore.Usage):
                UpdatePage("usage");
                break;
            case nameof(RuntimeStore.ClientKey):
                UpdatePage("overview");
                break;
            case nameof(RuntimeStore.State):
                UpdatePage("overview");
                UpdatePage("settings");
                break;
            case nameof(RuntimeStore.IsRuntimeAvailable):
            case null:
                foreach (var pageName in pages.Keys) UpdatePage(pageName);
                break;
        }
        App.Trace($"update:{propertyName ?? "all"}:completed");
    }

    private void UpdatePage(string pageName)
    {
        if (!pages.TryGetValue(pageName, out var nativePage)) return;
        App.Trace($"page:{pageName}:updating");
        nativePage.Update();
        App.Trace($"page:{pageName}:updated");
    }

    private void ShowPage(string pageName)
    {
        if (!pages.TryGetValue(pageName, out var nativePage))
        {
            nativePage = NativeViews.CreatePage(pageName, store, this);
            pages.Add(pageName, nativePage);
        }
        else UpdatePage(pageName);
        if (!ReferenceEquals(pageHost.Content, nativePage.Content)) pageHost.Content = nativePage.Content;
    }

    private async Task<(bool NavigationSucceeded, bool PageLoaded)> VerifySmokeUiAsync()
    {
        using var timeout = new CancellationTokenSource(TimeSpan.FromSeconds(10));
        try
        {
            await navigationInitialized.Task.WaitAsync(timeout.Token);
            foreach (var pageName in new[] { "overview", "agents", "usage", "settings" })
            {
                App.Trace($"smoke:navigating:{pageName}");
                navigation.SelectedItem = NavigationTarget(pageName);
                if (page != pageName ||
                    !pages.TryGetValue(pageName, out var nativePage) ||
                    !ReferenceEquals(pageHost.Content, nativePage.Content) ||
                    nativePage.Content is not FrameworkElement content)
                {
                    App.Trace($"smoke:navigation-failed:{pageName}");
                    return (false, false);
                }
                await WaitForLoadedAsync(content, timeout.Token);
                App.Trace($"smoke:page-loaded:{pageName}");
            }
            return (true, true);
        }
        catch (OperationCanceledException)
        {
            App.Trace("smoke:page-load-timeout");
            return (false, false);
        }
        catch (Exception error)
        {
            App.Trace($"smoke:navigation-error:{error.GetType().Name}");
            return (false, false);
        }
    }

    private object NavigationTarget(string pageName)
    {
        if (pageName == "settings") return navigation.SettingsItem;
        return navigation.MenuItems
            .OfType<NavigationViewItem>()
            .First(item => string.Equals(item.Tag?.ToString(), pageName, StringComparison.Ordinal));
    }

    private static async Task WaitForLoadedAsync(FrameworkElement content, CancellationToken cancellationToken)
    {
        if (content.IsLoaded) return;
        var loaded = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        RoutedEventHandler handler = (_, _) => loaded.TrySetResult(true);
        content.Loaded += handler;
        try { await loaded.Task.WaitAsync(cancellationToken); }
        finally { content.Loaded -= handler; }
    }

    private void Navigation_SelectionChanged(NavigationView sender, NavigationViewSelectionChangedEventArgs args)
    {
        App.Trace("navigation:selection-changing");
        page = args.IsSettingsSelected ? "settings" : (args.SelectedItemContainer?.Tag?.ToString() ?? "overview");
        pageTitle.Text = page switch { "agents" => "Agents", "usage" => "Usage", "settings" => "Settings", _ => "Overview" };
        if (!navigationReady)
        {
            App.Trace("navigation:selection-deferred");
            return;
        }
        ShowPage(page);
        App.Trace($"navigation:selection-changed:{page}");
    }

    private async void ProtectionSwitch_Toggled(object sender, RoutedEventArgs e)
    {
        if (syncingSwitch) return;
        if (protectionSwitch.IsOn && (!store.State.ApiKeySaved || store.State.Profiles.Length == 0))
        {
            syncingSwitch = true;
            protectionSwitch.IsOn = false;
            syncingSwitch = false;
            await ShowProfilesAsync();
            return;
        }
        try { await store.SetProtectionAsync(protectionSwitch.IsOn); }
        catch (Exception) { Update(null); }
    }

    private async void ToggleFromTray()
    {
        if (!store.IsRuntimeAvailable)
        {
            ShowMainWindow();
            ShowError("The desktop runtime is not running. Restart it to continue.");
            return;
        }
        if (!store.IsRunning && (!store.State.ApiKeySaved || store.State.Profiles.Length == 0))
        {
            ShowMainWindow();
            await ShowProfilesAsync();
            return;
        }
        try { await store.SetProtectionAsync(!store.IsRunning); }
        catch (Exception) { }
    }

    public Task ShowProfilesAsync() => NativeDialogs.ShowProfilesAsync(store, ContentRoot());
    public Task ShowLocalApiAsync() => NativeDialogs.ShowLocalApiAsync(store, ContentRoot());
    public Task ShowProofAsync(RequestActivity item) => NativeDialogs.ShowProofAsync(store.State, item, ContentRoot());
    public Task ShowPrivacyAsync() => NativeDialogs.ShowPrivacyAsync(store.State, ContentRoot());

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

    private async Task RestartRuntimeAsync()
    {
        restartButton.IsEnabled = false;
        var restarted = await store.RestartAsync();
        restartButton.IsEnabled = true;
        if (restarted) ClearError();
    }

    private void ShowError(string message)
    {
        errorBar.Title = store.IsRuntimeAvailable ? "Operation failed" : "Desktop runtime unavailable";
        errorBar.Message = message;
        restartButton.Visibility = store.IsRuntimeAvailable ? Visibility.Collapsed : Visibility.Visible;
        errorBar.IsClosable = store.IsRuntimeAvailable;
        errorBar.IsOpen = true;
    }

    private void ClearError()
    {
        errorBar.IsOpen = false;
        restartButton.Visibility = Visibility.Collapsed;
    }

    private void WriteHealthResult(bool healthy, bool navigationSucceeded, bool pageLoaded)
    {
        var path = Environment.GetEnvironmentVariable("PRIVATE_AI_GATEWAY_GUI_HEALTH");
        if (string.IsNullOrWhiteSpace(path)) return;
        var temporary = $"{path}.{Guid.NewGuid():N}.tmp";
        try
        {
            File.WriteAllText(temporary, JsonSerializer.Serialize(new
            {
                ok = healthy,
                runtimeAvailable = store.IsRuntimeAvailable,
                status = store.State.Status,
                agents = store.Agents.Length,
                clientKeyPresent = !string.IsNullOrEmpty(store.ClientKey),
                navigationSucceeded,
                pageLoaded,
            }));
            File.Move(temporary, path, true);
        }
        catch (Exception error) { App.Trace($"health-write-failed:{error.GetType().Name}"); }
        finally
        {
            try { File.Delete(temporary); }
            catch (Exception) { }
        }
    }

    private XamlRoot ContentRoot() => Content.XamlRoot;

    private async void QuitAsync() => await QuitCoreAsync();

    private async Task QuitCoreAsync()
    {
        if (quitting) return;
        quitting = true;
        App.Trace("quit:disposing-tray");
        tray.Dispose();
        App.Trace("quit:disposing-runtime");
        await store.DisposeAsync();
        App.Trace("quit:destroying-window");
        appWindow.Destroy();
        App.Trace("quit:exiting");
        Application.Current.Exit();
    }
}
