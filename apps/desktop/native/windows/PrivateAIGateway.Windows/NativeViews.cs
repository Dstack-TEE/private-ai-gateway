using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using global::Windows.Foundation;
using global::Windows.Storage.Pickers;
using WinRT.Interop;

namespace PrivateAIGateway.Windows;

internal static class NativeViews
{
    private static readonly SolidColorBrush Success = new(global::Windows.UI.Color.FromArgb(255, 44, 110, 73));
    private static readonly SolidColorBrush Warning = new(global::Windows.UI.Color.FromArgb(255, 184, 120, 0));

    internal static UIElement Overview(RuntimeStore store, MainWindow window)
    {
        var content = Vertical(28);
        content.Children.Add(ProtectionSurface(store, window));
        var columns = new Grid { ColumnSpacing = 20, RowSpacing = 42 };
        columns.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        columns.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        columns.RowDefinitions.Add(new() { Height = GridLength.Auto });
        columns.RowDefinitions.Add(new() { Height = GridLength.Auto });

        var local = Vertical(0);
        local.Children.Add(CopyRow("Endpoint", store.State.ProxyUrl ?? "Unavailable", store.State.ProxyUrl, "Available", () => _ = window.ShowLocalApiAsync()));
        local.Children.Add(new Border
        {
            Height = 1,
            Background = new SolidColorBrush(global::Windows.UI.Color.FromArgb(32, 128, 128, 128)),
        });
        local.Children.Add(ClientKeyRow(store));
        columns.Children.Add(OverviewModule("Local API", null, local, 136));

        var session = OverviewModule("Session usage", "This session", SessionMetrics(store.State.SessionUsage), 136);
        Grid.SetColumn(session, 1);
        columns.Children.Add(session);

        var agents = Vertical(0);
        foreach (var agent in store.Agents.Take(5)) agents.Children.Add(AgentRow(store, agent));
        var agentsModule = OverviewModule("Agents", "View all", agents, 320, () => window.NavigateTo("agents"));
        Grid.SetRow(agentsModule, 1);
        columns.Children.Add(agentsModule);

        var recent = Vertical(0);
        if (store.State.Activity.Length == 0) recent.Children.Add(Empty(store.IsRunning ? "No requests in this session yet." : "Start protection to begin a new session."));
        else foreach (var item in store.State.Activity.Take(5)) recent.Children.Add(UsageRow(item, () => _ = window.ShowProofAsync(item)));
        var recentModule = OverviewModule("Recent usage", "View all", recent, 320, () => window.NavigateTo("usage"));
        Grid.SetColumn(recentModule, 1);
        Grid.SetRow(recentModule, 1);
        columns.Children.Add(recentModule);

        content.Children.Add(columns);
        return Scroll(content);
    }

    private static UIElement ProtectionSurface(RuntimeStore store, MainWindow window)
    {
        var ready = store.IsProtected && store.State.ApiKeySaved;
        var root = new Grid { Height = 184 };
        root.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        root.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        root.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });

        if (ready)
        {
            root.Children.Add(TrackLayer(PlaintextTracks, false, 0.07, null));
            var cipher = TrackLayer(TlsTracks, true, 0.12, Success);
            Grid.SetColumn(cipher, 2);
            root.Children.Add(cipher);
        }

        var glow = new Border
        {
            Background = new RadialGradientBrush
            {
                Center = new Point(0.5, 0.5),
                GradientOrigin = new Point(0.5, 0.5),
                RadiusX = 0.24,
                RadiusY = 0.92,
                GradientStops =
                {
                    new GradientStop { Color = global::Windows.UI.Color.FromArgb(ready ? (byte)56 : (byte)0, 44, 110, 73), Offset = 0 },
                    new GradientStop { Color = global::Windows.UI.Color.FromArgb(ready ? (byte)28 : (byte)0, 44, 110, 73), Offset = 0.58 },
                    new GradientStop { Color = global::Windows.UI.Color.FromArgb(0, 44, 110, 73), Offset = 1 },
                },
            },
            IsHitTestVisible = false,
        };
        Grid.SetColumnSpan(glow, 3);
        root.Children.Add(glow);

        var local = Vertical(7);
        local.Padding = new Thickness(16);
        local.Children.Add(IconTitle("This PC", "\uE7F8"));
        local.Children.Add(Dim($"{store.Agents.Count(agent => agent.Recorded)} enabled · {store.Agents.Count(agent => agent.Connected)} active"));
        var agentIcons = Horizontal(6);
        foreach (var agent in store.Agents.Where(agent => agent.Recorded).Take(5)) agentIcons.Children.Add(AgentIcon(agent, 24, 15));
        local.Children.Add(agentIcons);
        local.Children.Add(new TextBlock { Text = "Enabled agents send their requests to the gateway on this PC.", FontSize = 12, Opacity = 0.62, TextWrapping = TextWrapping.Wrap, MaxWidth = 210 });
        root.Children.Add(local);

        var gateway = Vertical(4);
        gateway.HorizontalAlignment = HorizontalAlignment.Center;
        gateway.VerticalAlignment = VerticalAlignment.Center;
        gateway.Children.Add(AssetImage("Assets/brand/mark.svg", 44));
        gateway.Children.Add(new TextBlock { Text = "Private AI Gateway", FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, HorizontalAlignment = HorizontalAlignment.Center });
        gateway.Children.Add(new TextBlock
        {
            Text = store.IsDevMode && store.IsRunning ? "Protected · Dev mode" : Status(store.State),
            Foreground = store.IsDevMode && store.IsRunning ? Warning : store.IsProtected ? Success : null,
            FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
            HorizontalAlignment = HorizontalAlignment.Center,
        });
        var protection = new ToggleSwitch { IsOn = store.IsRunning, OffContent = "", OnContent = "", HorizontalAlignment = HorizontalAlignment.Center, IsEnabled = !store.IsBusy };
        protection.Toggled += async (_, _) =>
        {
            if (protection.IsOn == store.IsRunning) return;
            if (protection.IsOn && (!store.State.ApiKeySaved || store.State.Profiles.Length == 0))
            {
                protection.IsOn = false;
                await window.ShowProfilesAsync();
                return;
            }
            await store.SetProtectionAsync(protection.IsOn);
        };
        gateway.Children.Add(protection);
        Grid.SetColumn(gateway, 1);
        root.Children.Add(gateway);

        var remote = Vertical(6);
        remote.Padding = new Thickness(16);
        remote.HorizontalAlignment = HorizontalAlignment.Stretch;
        var service = Horizontal(8);
        service.HorizontalAlignment = HorizontalAlignment.Right;
        service.Children.Add(new TextBlock { Text = store.ActiveProfile?.Name ?? "Custom service", FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, VerticalAlignment = VerticalAlignment.Center });
        service.Children.Add(ServiceImage(store.ActiveProfile?.Provider, 24));
        remote.Children.Add(service);
        remote.Children.Add(new TextBlock { Text = ServiceHost(store.State.RemoteUrl ?? store.State.Config.RemoteUrl), FontFamily = new FontFamily("Consolas"), FontSize = 12, Opacity = 0.65, HorizontalAlignment = HorizontalAlignment.Right });
        remote.Children.Add(new TextBlock { Text = store.IsProtected ? "Verified hardware  ✓" : "Not verified  •", Foreground = store.IsProtected ? Success : null, FontSize = 12, HorizontalAlignment = HorizontalAlignment.Right });
        remote.Children.Add(new TextBlock { Text = store.IsProtected ? $"{store.State.SessionUsage.Protected:N0} answers this session  ✓" : "No answers this session  •", Foreground = store.IsProtected ? Success : null, FontSize = 12, HorizontalAlignment = HorizontalAlignment.Right });
        var actions = Horizontal(6);
        actions.HorizontalAlignment = HorizontalAlignment.Right;
        var profiles = IconButton("\uE713", "Profiles");
        profiles.Click += async (_, _) => await window.ShowProfilesAsync();
        var privacy = IconButton("\uEA18", "Privacy verification");
        privacy.IsEnabled = store.State.Identity is not null;
        privacy.Click += async (_, _) => await window.ShowPrivacyAsync();
        actions.Children.Add(profiles);
        actions.Children.Add(privacy);
        remote.Children.Add(actions);
        Grid.SetColumn(remote, 2);
        root.Children.Add(remote);

        return Surface(root);
    }

    internal static UIElement Agents(RuntimeStore store)
    {
        var content = Vertical(0);
        if (store.Agents.Length == 0) content.Children.Add(Empty("No supported agents were found"));
        else foreach (var agent in store.Agents) content.Children.Add(AgentRow(store, agent));
        return new ScrollViewer { Content = Card(content), Padding = new Thickness(24) };
    }

    internal static UIElement Usage(RuntimeStore store, MainWindow window)
    {
        var root = Vertical(18);
        var toolbar = Horizontal(12);
        toolbar.Children.Add(Filter("Agent", "All agents", store.Usage.Agents, store.UsageAgent, value => { store.UsageAgent = value; _ = store.ReloadUsageAsync(true); }));
        toolbar.Children.Add(Filter("Model", "All models", store.Usage.Models, store.UsageModel, value => { store.UsageModel = value; _ = store.ReloadUsageAsync(true); }));
        var range = new ComboBox { Header = "Time", MinWidth = 140 };
        foreach (var option in new[] { "7 days", "30 days", "All time" }) range.Items.Add(option);
        range.SelectedIndex = (int)store.UsageRange;
        range.SelectionChanged += (_, _) => { store.UsageRange = (UsageRange)Math.Max(0, range.SelectedIndex); _ = store.ReloadUsageAsync(true); };
        toolbar.Children.Add(range);
        toolbar.Children.Add(new Border { Width = 10 });
        var export = new Button { Content = "Export CSV…" };
        export.Click += async (_, _) => await ExportAsync(store, window);
        toolbar.Children.Add(export);
        var clear = new Button { Content = "Clear History…" };
        clear.Click += async (_, _) => await window.ConfirmClearUsageAsync();
        toolbar.Children.Add(clear);
        root.Children.Add(toolbar);
        root.Children.Add(Metrics(store.Usage.Summary, null));
        root.Children.Add(UsageChart(store.Usage.Series));
        var list = Section("Usage history");
        if (store.Usage.Items.Length == 0) list.Children.Add(Empty("No usage matches these filters"));
        else foreach (var item in store.Usage.Items) list.Children.Add(UsageRow(item, () => _ = window.ShowProofAsync(item)));
        root.Children.Add(Card(list));
        if (store.Usage.NextCursor is not null)
        {
            var more = new Button { Content = "Load More", HorizontalAlignment = HorizontalAlignment.Center };
            more.Click += async (_, _) => await store.ReloadUsageAsync(false);
            root.Children.Add(more);
        }
        return Scroll(root);
    }

    internal static UIElement Settings(RuntimeStore store, MainWindow window)
    {
        var root = Vertical(18);
        var profiles = Section("Confidential AI");
        profiles.Children.Add(new TextBlock { Text = store.ActiveProfile?.Name ?? "Not configured", FontWeight = Microsoft.UI.Text.FontWeights.SemiBold });
        var manage = new Button { Content = "Manage Profiles…", HorizontalAlignment = HorizontalAlignment.Left, IsEnabled = !store.IsRunning };
        manage.Click += async (_, _) => await window.ShowProfilesAsync();
        profiles.Children.Add(manage);
        root.Children.Add(Card(profiles));
        var local = Section("Local API");
        var localSettings = new Button { Content = "Local API Settings…", HorizontalAlignment = HorizontalAlignment.Left, IsEnabled = !store.IsRunning };
        localSettings.Click += async (_, _) => await window.ShowLocalApiAsync();
        var rotate = new Button { Content = "Rotate Client Key…", HorizontalAlignment = HorizontalAlignment.Left };
        rotate.Click += async (_, _) => await store.RotateClientKeyAsync();
        local.Children.Add(localSettings);
        local.Children.Add(rotate);
        root.Children.Add(Card(local));
        var advanced = new Expander { Header = "Advanced", IsExpanded = false };
        var advancedContent = Vertical(10);
        advancedContent.Children.Add(new TextBlock { Text = store.IsDevMode ? "Development OS allowed" : "Production OS required" });
        var restore = new Button { Content = "Restore All Agent Configurations…", HorizontalAlignment = HorizontalAlignment.Left };
        restore.Click += async (_, _) => await window.ConfirmRestoreAsync();
        advancedContent.Children.Add(restore);
        advanced.Content = advancedContent;
        root.Children.Add(advanced);
        return Scroll(root);
    }

    private static UIElement ProtectionSummary(RuntimeStore store, MainWindow window)
    {
        var grid = new Grid { Padding = new Thickness(20), ColumnSpacing = 16 };
        grid.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        var icon = new FontIcon { Glyph = store.IsProtected ? "\uE83D" : "\uEA18", FontSize = 32, Foreground = store.IsDevMode && store.IsRunning ? Warning : store.IsProtected ? Success : null };
        grid.Children.Add(icon);
        var text = Vertical(4);
        text.Children.Add(new TextBlock { Text = store.IsDevMode && store.IsRunning ? "Protected in dev mode" : Status(store.State), FontSize = 18, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold });
        text.Children.Add(new TextBlock { Text = store.State.Progress ?? store.State.Error ?? store.ActiveProfile?.Name ?? "Choose a Confidential AI profile", Opacity = 0.65, TextWrapping = TextWrapping.Wrap });
        Grid.SetColumn(text, 1); grid.Children.Add(text);
        var actions = Horizontal(8);
        var profile = new Button { Content = "Profiles…" }; profile.Click += async (_, _) => await window.ShowProfilesAsync();
        var privacy = new Button { Content = "Privacy Verification…", IsEnabled = store.State.Identity is not null }; privacy.Click += async (_, _) => await window.ShowPrivacyAsync();
        actions.Children.Add(profile); actions.Children.Add(privacy);
        Grid.SetColumn(actions, 2); grid.Children.Add(actions);
        return Card(grid);
    }

    private static UIElement AgentRow(RuntimeStore store, AgentStatus agent)
    {
        var grid = new Grid { Padding = new Thickness(12, 8, 12, 8), ColumnSpacing = 12 };
        grid.ColumnDefinitions.Add(new() { Width = new GridLength(30) });
        grid.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        grid.Children.Add(AssetImage($"Assets/agents/{agent.Id}.svg", 26));
        var labels = Vertical(2);
        labels.Children.Add(new TextBlock { Text = agent.Name });
        labels.Children.Add(new TextBlock { Text = agent.Error ?? agent.Attention ?? (agent.Installed ? agent.ConfigPath : "CLI not found"), FontSize = 12, Opacity = agent.Error is null ? 0.6 : 1, TextWrapping = TextWrapping.Wrap, MaxLines = 2 });
        Grid.SetColumn(labels, 1); grid.Children.Add(labels);
        var toggle = new ToggleSwitch { IsOn = agent.Connected, IsEnabled = agent.Installed || agent.Recorded, OffContent = "", OnContent = "" };
        toggle.Toggled += async (_, _) => { if (toggle.IsOn != agent.Connected) await store.SetAgentAsync(agent, toggle.IsOn); };
        Grid.SetColumn(toggle, 2); grid.Children.Add(toggle);
        return grid;
    }

    private static UIElement ClientKeyRow(RuntimeStore store)
    {
        var reveal = false;
        var value = new TextBlock { Text = "pag_••••••••••••", FontFamily = new FontFamily("Consolas") };
        var copy = new Button { HorizontalContentAlignment = HorizontalAlignment.Stretch, Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent), BorderThickness = new Thickness(0), Padding = new Thickness(12, 8, 92, 8), MinHeight = 67 };
        copy.Content = TitleValue("Client key", "for your own tools", value);
        copy.Click += (_, _) => global::Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(ClipboardContent(store.ClientKey));
        var eye = IconButton("\uE890", "Reveal client key");
        eye.Click += (_, _) => { reveal = !reveal; value.Text = reveal ? store.ClientKey : "pag_••••••••••••"; };
        var grid = new Grid();
        grid.Children.Add(copy);
        eye.HorizontalAlignment = HorizontalAlignment.Right;
        eye.Margin = new Thickness(0, 0, 12, 0);
        grid.Children.Add(eye);
        return grid;
    }

    private static UIElement CopyRow(string title, string value, string? copyValue, string side, Action action)
    {
        var valueBlock = new TextBlock { Text = value, FontFamily = new FontFamily("Consolas"), FontSize = 12, Opacity = 0.65, TextTrimming = TextTrimming.CharacterEllipsis };
        var button = new Button { HorizontalContentAlignment = HorizontalAlignment.Stretch, Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent), BorderThickness = new Thickness(0), IsEnabled = copyValue is not null, Padding = new Thickness(12, 8, 92, 8), MinHeight = 67 };
        button.Content = TitleValue(title, side, valueBlock);
        button.Click += (_, _) => { if (copyValue is not null) global::Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(ClipboardContent(copyValue)); };
        var settings = IconButton("\uE713", "Local API settings");
        settings.HorizontalAlignment = HorizontalAlignment.Right;
        settings.Margin = new Thickness(0, 0, 12, 0);
        settings.Click += (_, _) => action();
        var root = new Grid();
        root.Children.Add(button);
        root.Children.Add(settings);
        return root;
    }

    private static Border OverviewModule(string title, string? trailing, UIElement content, double contentHeight, Action? action = null)
    {
        var root = Vertical(8);
        var heading = new Grid();
        heading.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        heading.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        heading.Children.Add(new TextBlock { Text = title, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, VerticalAlignment = VerticalAlignment.Center });
        if (trailing is not null)
        {
            UIElement detail;
            if (action is null)
            {
                detail = new TextBlock { Text = trailing, FontSize = 12, Opacity = 0.65, VerticalAlignment = VerticalAlignment.Center };
            }
            else
            {
                var button = new Button { Content = trailing, Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent), BorderThickness = new Thickness(0), Foreground = Success, Padding = new Thickness(6, 2, 6, 2) };
                button.Click += (_, _) => action();
                detail = button;
            }
            Grid.SetColumn(detail, 1);
            heading.Children.Add(detail);
        }
        root.Children.Add(heading);
        var surface = Surface(content);
        surface.Height = contentHeight;
        root.Children.Add(surface);
        return new Border { Child = root };
    }

    private static UIElement SessionMetrics(UsageSummary summary)
    {
        var forwarded = Math.Max(0UL, summary.Requests - Math.Min(summary.Requests, summary.BlockedLocally));
        var values = new[]
        {
            ("Requests", $"{summary.Requests:N0}"),
            ("Tokens", CompactNumber(summary.InputTokens + summary.OutputTokens)),
            ("Cost", $"{summary.CostUsd:C}"),
            ("Protected", forwarded == 0 ? "—" : $"{summary.Protected * 100 / forwarded}%"),
        };
        var grid = new Grid();
        grid.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        grid.RowDefinitions.Add(new() { Height = new GridLength(1, GridUnitType.Star) });
        grid.RowDefinitions.Add(new() { Height = new GridLength(1, GridUnitType.Star) });
        for (var index = 0; index < values.Length; index++)
        {
            var metric = Vertical(1);
            metric.Padding = new Thickness(12, 7, 12, 7);
            metric.Children.Add(new TextBlock { Text = values[index].Item1, FontSize = 12, Opacity = 0.65 });
            metric.Children.Add(new TextBlock { Text = values[index].Item2, FontSize = 20, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold });
            var cell = new Border
            {
                Child = metric,
                BorderBrush = new SolidColorBrush(global::Windows.UI.Color.FromArgb(24, 128, 128, 128)),
                BorderThickness = new Thickness(index % 2 == 0 ? 0 : 1, index > 1 ? 1 : 0, 0, 0),
            };
            Grid.SetColumn(cell, index % 2);
            Grid.SetRow(cell, index / 2);
            grid.Children.Add(cell);
        }
        return grid;
    }

    private static UIElement TrackLayer(IEnumerable<string> lines, bool alignRight, double opacity, Brush? foreground)
    {
        var stack = Vertical(0);
        stack.Opacity = opacity;
        stack.IsHitTestVisible = false;
        foreach (var line in lines)
        {
            stack.Children.Add(new TextBlock
            {
                Text = line,
                FontFamily = new FontFamily("Consolas"),
                FontSize = 11,
                Foreground = foreground,
                TextTrimming = TextTrimming.CharacterEllipsis,
                TextAlignment = alignRight ? TextAlignment.Right : TextAlignment.Left,
                Height = 16,
            });
        }
        return stack;
    }

    private static StackPanel TitleValue(string title, string side, UIElement value)
    {
        var root = Vertical(2);
        var heading = new Grid();
        heading.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        heading.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        heading.Children.Add(new TextBlock { Text = title, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold });
        var sideText = new TextBlock { Text = side, FontSize = 12, Opacity = 0.65 };
        Grid.SetColumn(sideText, 1);
        heading.Children.Add(sideText);
        root.Children.Add(heading);
        root.Children.Add(value);
        return root;
    }

    private static StackPanel IconTitle(string title, string glyph)
    {
        var row = Horizontal(8);
        row.Children.Add(new FontIcon { Glyph = glyph, FontSize = 16 });
        row.Children.Add(new TextBlock { Text = title, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold });
        return row;
    }

    private static TextBlock Dim(string text) => new() { Text = text, FontSize = 12, Opacity = 0.65 };

    private static Border AgentIcon(AgentStatus agent, double size, double imageSize) => new()
    {
        Width = size,
        Height = size,
        Background = new SolidColorBrush(Microsoft.UI.Colors.White),
        BorderBrush = new SolidColorBrush(global::Windows.UI.Color.FromArgb(28, 0, 0, 0)),
        BorderThickness = new Thickness(1),
        CornerRadius = new CornerRadius(Math.Min(7, size / 4)),
        Child = AssetImage($"Assets/agents/{agent.Id}.svg", imageSize),
    };

    private static Image ServiceImage(string? provider, double size)
    {
        ImageSource? source = provider switch
        {
            "phala" => new SvgImageSource(new Uri("ms-appx:///Assets/providers/phala.svg")),
            "redpill" => new BitmapImage(new Uri("ms-appx:///Assets/providers/redpill.png")),
            _ => null,
        };
        return new Image { Source = source, Width = size, Height = size };
    }

    private static Button IconButton(string glyph, string tooltip)
    {
        var button = new Button { Content = new FontIcon { Glyph = glyph, FontSize = 14 }, Width = 32, Height = 30, Padding = new Thickness(0) };
        ToolTipService.SetToolTip(button, tooltip);
        return button;
    }

    private static string ServiceHost(string url) => Uri.TryCreate(url, UriKind.Absolute, out var value) ? value.Host : "Not configured";
    private static string CompactNumber(ulong value) => value switch
    {
        >= 1_000_000 => $"{value / 1_000_000d:0.#}M",
        >= 1_000 => $"{value / 1_000d:0.#}K",
        _ => $"{value:N0}",
    };

    private static UIElement Metrics(UsageSummary summary, string? title)
    {
        var stack = Vertical(10);
        if (title is not null) stack.Children.Add(new TextBlock { Text = title, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold });
        var grid = new Grid { ColumnSpacing = 20 };
        for (var index = 0; index < 4; index++) grid.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        var values = new[] { ("Requests", $"{summary.Requests:N0}"), ("Tokens", $"{summary.InputTokens + summary.OutputTokens:N0}"), ("Cost", $"{summary.CostUsd:C}"), ("Protected", summary.Requests == 0 ? "—" : $"{summary.Protected * 100 / summary.Requests}%") };
        for (var index = 0; index < values.Length; index++) { var metric = Labeled(values[index].Item1, new TextBlock { Text = values[index].Item2, FontSize = 18 }); Grid.SetColumn(metric, index); grid.Children.Add(metric); }
        stack.Children.Add(grid); return Card(stack);
    }

    private static UIElement UsageChart(UsagePoint[] points)
    {
        if (points.Length == 0) return Empty("No usage matches these filters");
        var max = Math.Max(1, points.Max(point => point.Tokens));
        var chart = new Grid { Height = 150, ColumnSpacing = 3, VerticalAlignment = VerticalAlignment.Bottom };
        for (var index = 0; index < points.Length; index++)
        {
            chart.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
            var column = new Grid { VerticalAlignment = VerticalAlignment.Bottom, Height = Math.Max(2, 120d * points[index].Tokens / max), Background = Success, CornerRadius = new CornerRadius(2) };
            ToolTipService.SetToolTip(column, $"{points[index].Day}: {points[index].Tokens:N0} tokens");
            Grid.SetColumn(column, index); chart.Children.Add(column);
        }
        return Card(chart);
    }

    private static UIElement UsageRow(RequestActivity item, Action action)
    {
        var button = new Button { HorizontalContentAlignment = HorizontalAlignment.Stretch, Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent), BorderThickness = new Thickness(0), Padding = new Thickness(12, 9, 12, 9) };
        var grid = new Grid { ColumnSpacing = 12 };
        grid.ColumnDefinitions.Add(new() { Width = new GridLength(24) }); grid.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) }); grid.ColumnDefinitions.Add(new() { Width = new GridLength(90) }); grid.ColumnDefinitions.Add(new() { Width = new GridLength(72) }); grid.ColumnDefinitions.Add(new() { Width = new GridLength(18) });
        grid.Children.Add(new FontIcon { Glyph = item.Verified == true ? "\uE83D" : item.LeftDevice ? "\uE814" : "\uE711", Foreground = item.Verified == true ? Success : item.LeftDevice ? new SolidColorBrush(Microsoft.UI.Colors.IndianRed) : null });
        var label = Vertical(2); label.Children.Add(new TextBlock { Text = item.Model ?? item.Path, TextTrimming = TextTrimming.CharacterEllipsis }); label.Children.Add(new TextBlock { Text = string.Join(" · ", new[] { item.Agent, item.Path }.Where(value => !string.IsNullOrEmpty(value))), FontSize = 12, Opacity = 0.6, TextTrimming = TextTrimming.CharacterEllipsis }); Grid.SetColumn(label, 1); grid.Children.Add(label);
        var tokens = new TextBlock { Text = $"{(item.InputTokens ?? 0) + (item.OutputTokens ?? 0):N0}", Opacity = 0.65, HorizontalAlignment = HorizontalAlignment.Right }; Grid.SetColumn(tokens, 2); grid.Children.Add(tokens);
        var time = new TextBlock { Text = DateTimeOffset.FromUnixTimeSeconds((long)item.At).ToLocalTime().ToString("t"), Opacity = 0.65, HorizontalAlignment = HorizontalAlignment.Right }; Grid.SetColumn(time, 3); grid.Children.Add(time);
        var arrow = new FontIcon { Glyph = "\uE76C", FontSize = 12, Opacity = 0.5 }; Grid.SetColumn(arrow, 4); grid.Children.Add(arrow);
        button.Content = grid; button.Click += (_, _) => action(); return button;
    }

    private static StackPanel Section(string title) { var stack = Vertical(0); stack.Children.Add(new TextBlock { Text = title, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, Margin = new Thickness(12, 10, 12, 7) }); return stack; }
    private static Border Card(UIElement content) => new() { Child = content, Background = new SolidColorBrush(global::Windows.UI.Color.FromArgb(20, 128, 128, 128)), BorderBrush = new SolidColorBrush(global::Windows.UI.Color.FromArgb(35, 128, 128, 128)), BorderThickness = new Thickness(1), CornerRadius = new CornerRadius(6), Padding = new Thickness(12) };
    private static Border Surface(UIElement content) => new() { Child = content, Background = ThemeBrush("CardBackgroundFillColorDefaultBrush", global::Windows.UI.Color.FromArgb(250, 255, 255, 255)), BorderBrush = ThemeBrush("CardStrokeColorDefaultBrush", global::Windows.UI.Color.FromArgb(28, 0, 0, 0)), BorderThickness = new Thickness(1), CornerRadius = new CornerRadius(6) };
    private static Border CardInto(Grid grid, UIElement content) { var card = Card(content); grid.Children.Add(card); return card; }
    private static StackPanel Vertical(double spacing) => new() { Orientation = Orientation.Vertical, Spacing = spacing };
    private static StackPanel Horizontal(double spacing) => new() { Orientation = Orientation.Horizontal, Spacing = spacing, VerticalAlignment = VerticalAlignment.Center };
    private static ScrollViewer Scroll(UIElement content) => new() { Content = content, Padding = new Thickness(24), HorizontalScrollMode = ScrollMode.Disabled };
    private static TextBlock Empty(string text) => new() { Text = text, HorizontalAlignment = HorizontalAlignment.Center, Opacity = 0.6, Margin = new Thickness(18, 28, 18, 28) };
    private static StackPanel Labeled(string title, UIElement value) { var stack = Vertical(3); stack.Children.Add(new TextBlock { Text = title, FontSize = 12, Opacity = 0.6 }); stack.Children.Add(value); return stack; }
    private static Image AssetImage(string source, double size) => new() { Source = new SvgImageSource(new Uri($"ms-appx:///{source}")), Width = size, Height = size };
    private static Brush ThemeBrush(string key, global::Windows.UI.Color fallback) => Application.Current.Resources.TryGetValue(key, out var value) && value is Brush brush ? brush : new SolidColorBrush(fallback);
    private static global::Windows.ApplicationModel.DataTransfer.DataPackage ClipboardContent(string value) { var package = new global::Windows.ApplicationModel.DataTransfer.DataPackage(); package.SetText(value); return package; }
    private static string Status(GatewayState state) => state.Status switch { "verified" => "Protected", "verifying" => "Verifying…", "blocked" => "Blocked", "error" => "Needs attention", _ => "Not protected" };

    private static readonly string[] PlaintextTracks =
    {
        "POST /v1/messages  model demo/verified-chat-01  stream true",
        "event content_block_delta  compose hash matches expected value",
        "messages user  inspect public dstack attestation report",
        "event message_delta  stop_reason end_turn  output_tokens 96",
        "POST /v1/responses  model demo/verified-reasoning-01",
        "event response.output_text.delta  release notes summarized",
        "input user  summarize public release notes  tool read_public_file",
        "event response.completed  input_tokens 384  output_tokens 96",
        "POST /v1/chat/completions  compare tdx_quote and compose_hash",
        "data chat.completion.chunk  both digests match",
        "tools function compare_hash  tool_choice auto",
    };

    private static readonly string[] TlsTracks =
    {
        "17 03 03 00 f4  9f3a c1e0 7b42 d5a8 0e6f 2c91",
        "17 03 03 03 1a  4d17 e8b3 5a0c f9d2 61b7 a3e4",
        "application_data record_len 244  17 03 03 00 f4  6d03 c1e8",
        "17 03 03 01 6c  e1a7 5c09 f38d 2b64 d0e7 4a1f",
        "17 03 03 00 5e  b2a0 7e95 0d1b 9c6e 18e4 a0f7",
        "application_data record_len 794  17 03 03 03 1a  b5c8 02a6",
        "17 03 03 02 48  9e21 4fb7 a6d5 0c83 e2f9 71b4",
        "17 03 03 00 91  81c7 e6a2 5b9d 1f74 c0e3 a8d6",
        "17 03 03 01 d0  a3e8 5c02 e9b1 4d7f 8a36 1e0c",
        "application_data record_len 152  17 03 03 00 98  c7d2 3e5a",
        "17 03 03 00 3c  7a0d 2c95 f6e3 41b8 d9c0 3f5e",
    };

    private static ComboBox Filter(string header, string all, IEnumerable<string> options, string? selected, Action<string?> changed)
    {
        var box = new ComboBox { Header = header, MinWidth = 150 };
        box.Items.Add(all); foreach (var option in options) box.Items.Add(option);
        box.SelectedIndex = selected is null ? 0 : Math.Max(0, Array.IndexOf(options.ToArray(), selected) + 1);
        box.SelectionChanged += (_, _) => changed(box.SelectedIndex <= 0 ? null : box.SelectedItem?.ToString()); return box;
    }

    private static async Task ExportAsync(RuntimeStore store, MainWindow window)
    {
        var picker = new FileSavePicker { SuggestedFileName = $"private-ai-gateway-usage-{DateTime.Now:yyyy-MM-dd}" };
        picker.FileTypeChoices.Add("CSV", new List<string> { ".csv" });
        InitializeWithWindow.Initialize(picker, WindowNative.GetWindowHandle(window));
        var file = await picker.PickSaveFileAsync(); if (file is not null) await store.ExportUsageAsync(file.Path);
    }
}
