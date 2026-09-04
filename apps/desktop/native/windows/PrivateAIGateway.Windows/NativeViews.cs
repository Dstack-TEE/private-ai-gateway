using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using global::Windows.Storage.Pickers;
using WinRT.Interop;

namespace PrivateAIGateway.Windows;

internal static class NativeViews
{
    private static readonly SolidColorBrush Success = new(global::Windows.UI.Color.FromArgb(255, 44, 110, 73));
    private static readonly SolidColorBrush Warning = new(global::Windows.UI.Color.FromArgb(255, 184, 120, 0));

    internal static UIElement Overview(RuntimeStore store, MainWindow window)
    {
        var content = Vertical(24);
        content.Children.Add(ProtectionSummary(store, window));
        var columns = new Grid { ColumnSpacing = 20 };
        columns.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        columns.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        var local = Section("Local API");
        local.Children.Add(CopyRow("Endpoint", store.State.ProxyUrl ?? "Unavailable", store.State.ProxyUrl));
        local.Children.Add(new Border
        {
            Height = 1,
            Margin = new Thickness(0, 4, 0, 4),
            Background = new SolidColorBrush(global::Windows.UI.Color.FromArgb(32, 128, 128, 128)),
        });
        local.Children.Add(ClientKeyRow(store));
        var agents = Section("Agents");
        foreach (var agent in store.Agents.Take(5)) agents.Children.Add(AgentRow(store, agent));
        columns.Children.Add(Card(local));
        Grid.SetColumn(CardInto(columns, agents), 1);
        content.Children.Add(columns);
        content.Children.Add(Metrics(store.State.SessionUsage, "This session"));
        var recent = Section("Recent usage");
        if (store.State.Activity.Length == 0) recent.Children.Add(Empty("No usage this session"));
        else foreach (var item in store.State.Activity.Take(5)) recent.Children.Add(UsageRow(item, () => _ = window.ShowProofAsync(item)));
        content.Children.Add(Card(recent));
        return Scroll(content);
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
        var copy = new Button { HorizontalContentAlignment = HorizontalAlignment.Stretch, Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent), BorderThickness = new Thickness(0) };
        copy.Content = Labeled("Client key", value);
        copy.Click += (_, _) => global::Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(ClipboardContent(store.ClientKey));
        var eye = new Button { Content = new FontIcon { Glyph = "\uE890" } };
        eye.Click += (_, _) => { reveal = !reveal; value.Text = reveal ? store.ClientKey : "pag_••••••••••••"; };
        var grid = new Grid { ColumnSpacing = 8 };
        grid.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        grid.Children.Add(copy); Grid.SetColumn(eye, 1); grid.Children.Add(eye);
        return grid;
    }

    private static UIElement CopyRow(string title, string value, string? copyValue)
    {
        var button = new Button { HorizontalContentAlignment = HorizontalAlignment.Stretch, Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent), BorderThickness = new Thickness(0), IsEnabled = copyValue is not null };
        var grid = new Grid(); grid.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) }); grid.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
        grid.Children.Add(Labeled(title, new TextBlock { Text = value, FontFamily = new FontFamily("Consolas"), TextTrimming = TextTrimming.CharacterEllipsis }));
        var icon = new FontIcon { Glyph = "\uE8C8" }; Grid.SetColumn(icon, 1); grid.Children.Add(icon); button.Content = grid;
        button.Click += (_, _) => { if (copyValue is not null) global::Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(ClipboardContent(copyValue)); };
        return button;
    }

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
    private static Border CardInto(Grid grid, UIElement content) { var card = Card(content); grid.Children.Add(card); return card; }
    private static StackPanel Vertical(double spacing) => new() { Orientation = Orientation.Vertical, Spacing = spacing };
    private static StackPanel Horizontal(double spacing) => new() { Orientation = Orientation.Horizontal, Spacing = spacing, VerticalAlignment = VerticalAlignment.Center };
    private static ScrollViewer Scroll(UIElement content) => new() { Content = content, Padding = new Thickness(24), HorizontalScrollMode = ScrollMode.Disabled };
    private static TextBlock Empty(string text) => new() { Text = text, HorizontalAlignment = HorizontalAlignment.Center, Opacity = 0.6, Margin = new Thickness(18, 28, 18, 28) };
    private static StackPanel Labeled(string title, UIElement value) { var stack = Vertical(3); stack.Children.Add(new TextBlock { Text = title, FontSize = 12, Opacity = 0.6 }); stack.Children.Add(value); return stack; }
    private static Image AssetImage(string source, double size) => new() { Source = new SvgImageSource(new Uri($"ms-appx:///{source}")), Width = size, Height = size };
    private static global::Windows.ApplicationModel.DataTransfer.DataPackage ClipboardContent(string value) { var package = new global::Windows.ApplicationModel.DataTransfer.DataPackage(); package.SetText(value); return package; }
    private static string Status(GatewayState state) => state.Status switch { "verified" => "Protected", "verifying" => "Verifying…", "blocked" => "Blocked", "error" => "Needs attention", _ => "Not protected" };

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
