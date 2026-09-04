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
    private readonly NavigationView Navigation = new();
    private readonly TextBlock PageTitle = new() { Text = "Overview", FontSize = 20, FontWeight = global::Microsoft.UI.Text.FontWeights.SemiBold, VerticalAlignment = VerticalAlignment.Center };
    private readonly Border DevBadge = new() { Background = new SolidColorBrush(ColorHelper.FromArgb(0x33, 0xE9, 0xA4, 0)), CornerRadius = new CornerRadius(4), Padding = new Thickness(7, 3, 7, 3), Visibility = Visibility.Collapsed };
    private readonly TextBlock StatusText = new() { VerticalAlignment = VerticalAlignment.Center, Opacity = 0.7 };
    private readonly ToggleSwitch ProtectionSwitch = new();
    private readonly ContentPresenter PageHost = new();
    private string page = "overview";
    private bool syncingSwitch;
    private bool initialized;
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
        appWindow.Closing += (_, args) => { args.Cancel = true; appWindow.Hide(); };
        App.Trace("window:app-window-ready");
        tray = new NativeTray(hwnd, ShowMainWindow, ShowSettings, ToggleFromTray, QuitAsync);
        App.Trace("window:tray-ready");
        store.PropertyChanged += (_, _) => DispatcherQueue.TryEnqueue(Render);
        store.Error += message => DispatcherQueue.TryEnqueue(async () => await ShowErrorAsync(message));
        Activated += async (_, _) => { if (!initialized) { initialized = true; await InitializeAsync(); } };
        Navigation.Loaded += SelectInitialPage;
        App.Trace("window:constructed");
    }

    private void SelectInitialPage(object sender, RoutedEventArgs args)
    {
        Navigation.Loaded -= SelectInitialPage;
        App.Trace("navigation:loaded");
        DispatcherQueue.TryEnqueue(() =>
        {
            App.Trace("navigation:selecting");
            Navigation.SelectedItem = Navigation.MenuItems[0];
            App.Trace("navigation:selected");
        });
    }

    private void BuildShell()
    {
        Navigation.IsBackButtonVisible = NavigationViewBackButtonVisible.Collapsed;
        Navigation.IsSettingsVisible = true;
        Navigation.PaneDisplayMode = NavigationViewPaneDisplayMode.Left;
        Navigation.OpenPaneLength = 220;
        Navigation.SelectionChanged += Navigation_SelectionChanged;
        Navigation.MenuItems.Add(NavigationItem("Overview", "overview", new SymbolIcon(Symbol.Home)));
        Navigation.MenuItems.Add(NavigationItem("Agents", "agents", new FontIcon { Glyph = "\uE756" }));
        Navigation.MenuItems.Add(NavigationItem("Usage", "usage", new FontIcon { Glyph = "\uE9D2" }));

        var brand = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 10, Padding = new Thickness(4, 8, 0, 16) };
        brand.Children.Add(new Image
        {
            Source = new SvgImageSource(new Uri("ms-appx:///Assets/brand/mark.svg")),
            Width = 34,
            Height = 34,
        });
        var brandText = new StackPanel { VerticalAlignment = VerticalAlignment.Center };
        brandText.Children.Add(new TextBlock { Text = "Private AI Gateway", FontWeight = global::Microsoft.UI.Text.FontWeights.SemiBold });
        brandText.Children.Add(new TextBlock { Text = "Confidential inference", FontSize = 12, Opacity = 0.65 });
        brand.Children.Add(brandText);
        Navigation.PaneHeader = brand;

        var shell = new Grid();
        shell.RowDefinitions.Add(new RowDefinition { Height = new GridLength(58) });
        shell.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        var header = new Grid { Padding = new Thickness(24, 0, 24, 0) };
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        header.Children.Add(PageTitle);

        DevBadge.Child = new TextBlock { Text = "Dev mode", Foreground = new SolidColorBrush(ColorHelper.FromArgb(0xFF, 0xB8, 0x78, 0)), FontWeight = global::Microsoft.UI.Text.FontWeights.SemiBold };
        var controls = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 12, VerticalAlignment = VerticalAlignment.Center };
        controls.Children.Add(DevBadge);
        controls.Children.Add(StatusText);
        controls.Children.Add(new TextBlock { Text = "Protected", VerticalAlignment = VerticalAlignment.Center });
        ProtectionSwitch.Toggled += ProtectionSwitch_Toggled;
        controls.Children.Add(ProtectionSwitch);
        Grid.SetColumn(controls, 1);
        header.Children.Add(controls);

        var headerBorder = new Border
        {
            BorderBrush = new SolidColorBrush(ColorHelper.FromArgb(0x20, 0x80, 0x80, 0x80)),
            BorderThickness = new Thickness(0, 0, 0, 1),
            Child = header,
        };
        shell.Children.Add(headerBorder);
        Grid.SetRow(PageHost, 1);
        shell.Children.Add(PageHost);
        Navigation.Content = shell;
        Content = Navigation;
    }

    private static NavigationViewItem NavigationItem(string title, string tag, IconElement icon) => new()
    {
        Content = title,
        Tag = tag,
        Icon = icon,
    };

    public void ShowMainWindow()
    {
        appWindow.Show();
        Activate();
    }

    public void HideMainWindow() => appWindow.Hide();

    public void HideAfterLaunch() => DispatcherQueue.TryEnqueue(() =>
    {
        App.Trace("window:hiding");
        appWindow.Hide();
        App.Trace("window:hidden");
    });

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
