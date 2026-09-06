using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using global::Windows.Storage.Pickers;
using WinRT.Interop;

namespace PrivateAIGateway.Windows;

internal interface INativePage
{
    UIElement Content { get; }
    void Update();
}

internal static class NativeViews
{
    private static readonly SolidColorBrush Success = new(global::Windows.UI.Color.FromArgb(255, 44, 110, 73));
    private static readonly SolidColorBrush Warning = new(global::Windows.UI.Color.FromArgb(255, 184, 120, 0));

    internal static IReadOnlyDictionary<string, INativePage> CreatePages(RuntimeStore store, MainWindow window) =>
        new Dictionary<string, INativePage>
        {
            ["overview"] = new OverviewPage(store, window),
            ["agents"] = new AgentsPage(store),
            ["usage"] = new UsagePageView(store, window),
            ["settings"] = new SettingsPage(store, window),
        };

    private sealed class OverviewPage : INativePage
    {
        private readonly RuntimeStore store;
        private readonly MainWindow window;
        private readonly FontIcon protectionIcon = new() { FontSize = 32 };
        private readonly TextBlock summaryTitle = new() { FontSize = 18, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold };
        private readonly TextBlock summaryDetail = new() { Opacity = 0.65, TextWrapping = TextWrapping.Wrap };
        private readonly Button privacy = new() { Content = "Privacy Verification…" };
        private readonly TextBlock endpoint = new() { FontFamily = new FontFamily("Consolas"), TextTrimming = TextTrimming.CharacterEllipsis };
        private readonly Button endpointCopy = new() { HorizontalContentAlignment = HorizontalAlignment.Stretch, Background = Transparent, BorderThickness = new Thickness(0) };
        private readonly TextBlock clientKey = new() { FontFamily = new FontFamily("Consolas") };
        private readonly AgentList agentRows;
        private readonly TextBlock[] sessionMetrics = MetricValues();
        private readonly UsageList recentRows;
        private bool revealKey;

        internal OverviewPage(RuntimeStore store, MainWindow window)
        {
            this.store = store;
            this.window = window;
            agentRows = new AgentList(store, "Agents");
            recentRows = new UsageList(window, "Recent usage");
            var content = Vertical(24);
            content.Children.Add(BuildSummary());
            var columns = new Grid { ColumnSpacing = 20 };
            columns.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
            columns.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
            columns.Children.Add(Card(BuildLocalApi()));
            var agents = Card(agentRows.Content);
            Grid.SetColumn(agents, 1);
            columns.Children.Add(agents);
            content.Children.Add(columns);
            content.Children.Add(Metrics(sessionMetrics, "This session"));
            content.Children.Add(Card(recentRows.Content));
            Content = Scroll(content);
            Update();
        }

        public UIElement Content { get; }

        public void Update()
        {
            var state = store.State;
            protectionIcon.Glyph = store.IsProtected ? "\uE83D" : "\uEA18";
            protectionIcon.Foreground = store.IsRuntimeAvailable && RuntimePresentation.ShowDevMode(state) ? Warning : store.IsProtected ? Success : null;
            summaryTitle.Text = store.IsRuntimeAvailable ? RuntimePresentation.SummaryLabel(state) : "Runtime unavailable";
            summaryDetail.Text = state.Progress ?? state.Error ?? store.ActiveProfile?.Name ?? "Choose a Confidential AI profile";
            privacy.IsEnabled = state.Identity is not null && store.IsRuntimeAvailable;
            endpoint.Text = state.ProxyUrl ?? "Unavailable";
            endpointCopy.IsEnabled = state.ProxyUrl is not null && store.IsRuntimeAvailable;
            clientKey.Text = revealKey ? store.ClientKey : "pag_••••••••••••";
            agentRows.Reconcile(store.Agents.Take(5));
            SetMetrics(sessionMetrics, state.SessionUsage);
            recentRows.Reconcile(state.Activity.Take(5), "No usage this session");
        }

        private UIElement BuildSummary()
        {
            var grid = new Grid { Padding = new Thickness(20), ColumnSpacing = 16 };
            grid.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
            grid.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
            grid.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
            grid.Children.Add(protectionIcon);
            var text = Vertical(4);
            text.Children.Add(summaryTitle);
            text.Children.Add(summaryDetail);
            Grid.SetColumn(text, 1);
            grid.Children.Add(text);
            var actions = Horizontal(8);
            var profiles = new Button { Content = "Profiles…" };
            profiles.Click += async (_, _) => await window.ShowProfilesAsync();
            privacy.Click += async (_, _) => await window.ShowPrivacyAsync();
            actions.Children.Add(profiles);
            actions.Children.Add(privacy);
            Grid.SetColumn(actions, 2);
            grid.Children.Add(actions);
            return Card(grid);
        }

        private UIElement BuildLocalApi()
        {
            var local = Section("Local API");
            var endpointGrid = new Grid();
            endpointGrid.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
            endpointGrid.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
            endpointGrid.Children.Add(Labeled("Endpoint", endpoint));
            var copyIcon = new FontIcon { Glyph = "\uE8C8" };
            Grid.SetColumn(copyIcon, 1);
            endpointGrid.Children.Add(copyIcon);
            endpointCopy.Content = endpointGrid;
            endpointCopy.Click += (_, _) =>
            {
                if (store.State.ProxyUrl is { } value) global::Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(ClipboardContent(value));
            };
            local.Children.Add(endpointCopy);
            local.Children.Add(Divider());

            var keyCopy = new Button { HorizontalContentAlignment = HorizontalAlignment.Stretch, Background = Transparent, BorderThickness = new Thickness(0) };
            keyCopy.Content = Labeled("Client key", clientKey);
            keyCopy.Click += (_, _) => global::Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(ClipboardContent(store.ClientKey));
            var eye = new Button { Content = new FontIcon { Glyph = "\uE890" } };
            eye.Click += (_, _) =>
            {
                revealKey = !revealKey;
                clientKey.Text = revealKey ? store.ClientKey : "pag_••••••••••••";
                ToolTipService.SetToolTip(eye, revealKey ? "Hide client key" : "Reveal client key");
            };
            ToolTipService.SetToolTip(eye, "Reveal client key");
            var row = new Grid { ColumnSpacing = 8 };
            row.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
            row.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
            row.Children.Add(keyCopy);
            Grid.SetColumn(eye, 1);
            row.Children.Add(eye);
            local.Children.Add(row);
            return local;
        }
    }

    private sealed class AgentsPage : INativePage
    {
        private readonly RuntimeStore store;
        private readonly AgentList rows;

        internal AgentsPage(RuntimeStore store)
        {
            this.store = store;
            rows = new AgentList(store, null);
            Content = new ScrollViewer { Content = Card(rows.Content), Padding = new Thickness(24), HorizontalScrollMode = ScrollMode.Disabled };
            Update();
        }

        public UIElement Content { get; }
        public void Update() => rows.Reconcile(store.Agents);
    }

    private sealed class UsagePageView : INativePage
    {
        private readonly RuntimeStore store;
        private readonly MainWindow window;
        private readonly ComboBox agentFilter = new() { Header = "Agent", MinWidth = 150 };
        private readonly ComboBox modelFilter = new() { Header = "Model", MinWidth = 150 };
        private readonly ComboBox rangeFilter = new() { Header = "Time", MinWidth = 140 };
        private readonly TextBlock[] summaryMetrics = MetricValues();
        private readonly ContentControl chart = new();
        private readonly UsageList history;
        private readonly Button more = new() { Content = "Load More", HorizontalAlignment = HorizontalAlignment.Center };
        private bool syncingFilters;

        internal UsagePageView(RuntimeStore store, MainWindow window)
        {
            this.store = store;
            this.window = window;
            history = new UsageList(window, "Usage history");
            agentFilter.SelectionChanged += async (_, _) => await FilterChangedAsync(true);
            modelFilter.SelectionChanged += async (_, _) => await FilterChangedAsync(false);
            foreach (var option in new[] { "7 days", "30 days", "All time" }) rangeFilter.Items.Add(option);
            rangeFilter.SelectionChanged += async (_, _) =>
            {
                if (syncingFilters) return;
                store.UsageRange = (UsageRange)Math.Max(0, rangeFilter.SelectedIndex);
                try { await store.ReloadUsageAsync(true); } catch (RuntimeException) { }
            };
            more.Click += async (_, _) =>
            {
                try { await store.ReloadUsageAsync(false); } catch (RuntimeException) { }
            };

            var root = Vertical(18);
            var toolbar = Horizontal(12);
            toolbar.Children.Add(agentFilter);
            toolbar.Children.Add(modelFilter);
            toolbar.Children.Add(rangeFilter);
            toolbar.Children.Add(new Border { Width = 10 });
            var export = new Button { Content = "Export CSV…" };
            export.Click += async (_, _) => await ExportAsync(store, window);
            var clear = new Button { Content = "Clear History…" };
            clear.Click += async (_, _) => await window.ConfirmClearUsageAsync();
            toolbar.Children.Add(export);
            toolbar.Children.Add(clear);
            root.Children.Add(toolbar);
            root.Children.Add(Metrics(summaryMetrics, null));
            root.Children.Add(chart);
            root.Children.Add(Card(history.Content));
            root.Children.Add(more);
            Content = Scroll(root);
            Update();
        }

        public UIElement Content { get; }

        public void Update()
        {
            syncingFilters = true;
            SetFilter(agentFilter, "All agents", store.Usage.Agents, store.UsageAgent);
            SetFilter(modelFilter, "All models", store.Usage.Models, store.UsageModel);
            rangeFilter.SelectedIndex = (int)store.UsageRange;
            syncingFilters = false;
            agentFilter.IsEnabled = store.IsRuntimeAvailable;
            modelFilter.IsEnabled = store.IsRuntimeAvailable;
            rangeFilter.IsEnabled = store.IsRuntimeAvailable;
            SetMetrics(summaryMetrics, store.Usage.Summary);
            chart.Content = UsageChart(store.Usage.Series);
            history.Reconcile(store.Usage.Items, "No usage matches these filters");
            more.Visibility = store.Usage.NextCursor is null ? Visibility.Collapsed : Visibility.Visible;
            more.IsEnabled = store.IsRuntimeAvailable;
        }

        private async Task FilterChangedAsync(bool agent)
        {
            if (syncingFilters) return;
            var box = agent ? agentFilter : modelFilter;
            var value = box.SelectedIndex <= 0 ? null : box.SelectedItem?.ToString();
            if (agent) store.UsageAgent = value;
            else store.UsageModel = value;
            try { await store.ReloadUsageAsync(true); } catch (RuntimeException) { }
        }
    }

    private sealed class SettingsPage : INativePage
    {
        private readonly RuntimeStore store;
        private readonly TextBlock profile = new() { FontWeight = Microsoft.UI.Text.FontWeights.SemiBold };
        private readonly Button manage = new() { Content = "Manage Profiles…", HorizontalAlignment = HorizontalAlignment.Left };
        private readonly Button localSettings = new() { Content = "Local API Settings…", HorizontalAlignment = HorizontalAlignment.Left };
        private readonly Button rotate = new() { Content = "Rotate Client Key…", HorizontalAlignment = HorizontalAlignment.Left };
        private readonly TextBlock policy = new();
        private readonly Button restore = new() { Content = "Restore All Agent Configurations…", HorizontalAlignment = HorizontalAlignment.Left };

        internal SettingsPage(RuntimeStore store, MainWindow window)
        {
            this.store = store;
            var root = Vertical(18);
            var profiles = Section("Confidential AI");
            profiles.Children.Add(profile);
            manage.Click += async (_, _) => await window.ShowProfilesAsync();
            profiles.Children.Add(manage);
            root.Children.Add(Card(profiles));
            var local = Section("Local API");
            localSettings.Click += async (_, _) => await window.ShowLocalApiAsync();
            rotate.Click += async (_, _) => await store.RotateClientKeyAsync();
            local.Children.Add(localSettings);
            local.Children.Add(rotate);
            root.Children.Add(Card(local));
            var advanced = new Expander { Header = "Advanced", IsExpanded = false };
            var advancedContent = Vertical(10);
            advancedContent.Children.Add(policy);
            restore.Click += async (_, _) => await window.ConfirmRestoreAsync();
            advancedContent.Children.Add(restore);
            advanced.Content = advancedContent;
            root.Children.Add(advanced);
            Content = Scroll(root);
            Update();
        }

        public UIElement Content { get; }

        public void Update()
        {
            profile.Text = store.ActiveProfile?.Name ?? "Not configured";
            manage.IsEnabled = store.IsRuntimeAvailable && !store.IsRunning;
            localSettings.IsEnabled = store.IsRuntimeAvailable && !store.IsRunning;
            rotate.IsEnabled = store.IsRuntimeAvailable;
            restore.IsEnabled = store.IsRuntimeAvailable;
            policy.Text = store.IsDevMode ? "Development OS allowed" : "Production OS required";
        }
    }

    private sealed class AgentList
    {
        private readonly RuntimeStore store;
        private readonly int offset;
        private readonly Dictionary<string, AgentRowView> rows = [];
        private readonly TextBlock empty = Empty("No supported agents were found");

        internal AgentList(RuntimeStore store, string? title)
        {
            this.store = store;
            Content = title is null ? Vertical(0) : Section(title);
            offset = title is null ? 0 : 1;
        }

        internal StackPanel Content { get; }

        internal void Reconcile(IEnumerable<AgentStatus> agents)
        {
            var values = agents.ToArray();
            var ids = values.Select(agent => agent.Id).ToHashSet(StringComparer.Ordinal);
            foreach (var stale in rows.Keys.Where(id => !ids.Contains(id)).ToArray())
            {
                Content.Children.Remove(rows[stale].Content);
                rows.Remove(stale);
            }
            Content.Children.Remove(empty);
            for (var index = 0; index < values.Length; index++)
            {
                var agent = values[index];
                if (!rows.TryGetValue(agent.Id, out var row)) rows[agent.Id] = row = new AgentRowView(store, agent);
                row.Update(agent);
                Place(Content, row.Content, offset + index);
            }
            if (values.Length == 0) Content.Children.Add(empty);
        }
    }

    private sealed class AgentRowView
    {
        private readonly RuntimeStore store;
        private readonly TextBlock name = new();
        private readonly TextBlock detail = new() { FontSize = 12, TextWrapping = TextWrapping.Wrap, MaxLines = 2 };
        private readonly ToggleSwitch toggle = new() { OffContent = "", OnContent = "" };
        private AgentStatus agent;
        private bool syncing;

        internal AgentRowView(RuntimeStore store, AgentStatus agent)
        {
            this.store = store;
            this.agent = agent;
            var grid = new Grid { Padding = new Thickness(12, 8, 12, 8), ColumnSpacing = 12 };
            grid.ColumnDefinitions.Add(new() { Width = new GridLength(30) });
            grid.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
            grid.ColumnDefinitions.Add(new() { Width = GridLength.Auto });
            grid.Children.Add(AssetImage($"Assets/agents/{agent.Id}.svg", 26));
            var labels = Vertical(2);
            labels.Children.Add(name);
            labels.Children.Add(detail);
            Grid.SetColumn(labels, 1);
            grid.Children.Add(labels);
            toggle.Toggled += async (_, _) =>
            {
                if (!syncing && toggle.IsOn != this.agent.Connected) await store.SetAgentAsync(this.agent, toggle.IsOn);
            };
            Grid.SetColumn(toggle, 2);
            grid.Children.Add(toggle);
            Content = grid;
            Update(agent);
        }

        internal UIElement Content { get; }

        internal void Update(AgentStatus next)
        {
            agent = next;
            name.Text = next.Name;
            detail.Text = next.Error ?? next.Attention ?? (next.Installed ? next.ConfigPath : "CLI not found");
            detail.Opacity = next.Error is null ? 0.6 : 1;
            syncing = true;
            toggle.IsOn = next.Connected;
            toggle.IsEnabled = store.IsRuntimeAvailable && (next.Installed || next.Recorded);
            syncing = false;
        }
    }

    private sealed class UsageList
    {
        private readonly MainWindow window;
        private readonly int offset;
        private readonly Dictionary<string, UsageRowView> rows = [];
        private TextBlock? empty;

        internal UsageList(MainWindow window, string? title)
        {
            this.window = window;
            Content = title is null ? Vertical(0) : Section(title);
            offset = title is null ? 0 : 1;
        }

        internal StackPanel Content { get; }

        internal void Reconcile(IEnumerable<RequestActivity> items, string emptyText)
        {
            var values = items.ToArray();
            var ids = values.Select(item => item.Id).ToHashSet(StringComparer.Ordinal);
            foreach (var stale in rows.Keys.Where(id => !ids.Contains(id)).ToArray())
            {
                Content.Children.Remove(rows[stale].Content);
                rows.Remove(stale);
            }
            if (empty is not null) Content.Children.Remove(empty);
            for (var index = 0; index < values.Length; index++)
            {
                var item = values[index];
                if (!rows.TryGetValue(item.Id, out var row)) rows[item.Id] = row = new UsageRowView(window, item);
                row.Update(item);
                Place(Content, row.Content, offset + index);
            }
            if (values.Length == 0)
            {
                empty = Empty(emptyText);
                Content.Children.Add(empty);
            }
        }
    }

    private static TextBlock[] MetricValues() => Enumerable.Range(0, 4).Select(_ => new TextBlock { FontSize = 18 }).ToArray();

    private static UIElement Metrics(TextBlock[] values, string? title)
    {
        var stack = Vertical(10);
        if (title is not null) stack.Children.Add(new TextBlock { Text = title, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold });
        var grid = new Grid { ColumnSpacing = 20 };
        for (var index = 0; index < values.Length; index++) grid.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        var labels = new[] { "Requests", "Tokens", "Cost", "Protected" };
        for (var index = 0; index < values.Length; index++)
        {
            var metric = Labeled(labels[index], values[index]);
            Grid.SetColumn(metric, index);
            grid.Children.Add(metric);
        }
        stack.Children.Add(grid);
        return Card(stack);
    }

    private static void SetMetrics(TextBlock[] values, UsageSummary summary)
    {
        values[0].Text = $"{summary.Requests:N0}";
        values[1].Text = $"{summary.InputTokens + summary.OutputTokens:N0}";
        values[2].Text = $"{summary.CostUsd:C}";
        values[3].Text = summary.Requests == 0 ? "—" : $"{summary.Protected * 100 / summary.Requests}%";
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
            Grid.SetColumn(column, index);
            chart.Children.Add(column);
        }
        return Card(chart);
    }

    private sealed class UsageRowView
    {
        private readonly FontIcon verdict = new();
        private readonly TextBlock title = new() { TextTrimming = TextTrimming.CharacterEllipsis };
        private readonly TextBlock subtitle = new() { FontSize = 12, Opacity = 0.6, TextTrimming = TextTrimming.CharacterEllipsis };
        private readonly TextBlock tokens = new() { Opacity = 0.65, HorizontalAlignment = HorizontalAlignment.Right };
        private readonly TextBlock time = new() { Opacity = 0.65, HorizontalAlignment = HorizontalAlignment.Right };
        private RequestActivity item;

        internal UsageRowView(MainWindow window, RequestActivity item)
        {
            this.item = item;
            var button = new Button { HorizontalContentAlignment = HorizontalAlignment.Stretch, Background = Transparent, BorderThickness = new Thickness(0), Padding = new Thickness(12, 9, 12, 9) };
            var grid = new Grid { ColumnSpacing = 12 };
            grid.ColumnDefinitions.Add(new() { Width = new GridLength(24) });
            grid.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
            grid.ColumnDefinitions.Add(new() { Width = new GridLength(90) });
            grid.ColumnDefinitions.Add(new() { Width = new GridLength(72) });
            grid.ColumnDefinitions.Add(new() { Width = new GridLength(18) });
            grid.Children.Add(verdict);
            var label = Vertical(2);
            label.Children.Add(title);
            label.Children.Add(subtitle);
            Grid.SetColumn(label, 1);
            grid.Children.Add(label);
            Grid.SetColumn(tokens, 2);
            grid.Children.Add(tokens);
            Grid.SetColumn(time, 3);
            grid.Children.Add(time);
            var arrow = new FontIcon { Glyph = "\uE76C", FontSize = 12, Opacity = 0.5 };
            Grid.SetColumn(arrow, 4);
            grid.Children.Add(arrow);
            button.Content = grid;
            button.Click += (_, _) => _ = window.ShowProofAsync(this.item);
            Content = button;
            Update(item);
        }

        internal UIElement Content { get; }

        internal void Update(RequestActivity next)
        {
            item = next;
            verdict.Glyph = next.Verified == true ? "\uE83D" : next.LeftDevice ? "\uE814" : "\uE711";
            verdict.Foreground = next.Verified == true ? Success : next.LeftDevice ? new SolidColorBrush(Microsoft.UI.Colors.IndianRed) : null;
            title.Text = next.Model ?? next.Path;
            subtitle.Text = string.Join(" · ", new[] { next.Agent, next.Path }.Where(value => !string.IsNullOrEmpty(value)));
            tokens.Text = $"{(next.InputTokens ?? 0) + (next.OutputTokens ?? 0):N0}";
            time.Text = DateTimeOffset.FromUnixTimeSeconds((long)next.At).ToLocalTime().ToString("t");
        }
    }

    private static void Place(StackPanel parent, UIElement child, int index)
    {
        var current = parent.Children.IndexOf(child);
        if (current == index) return;
        if (current >= 0) parent.Children.RemoveAt(current);
        parent.Children.Insert(Math.Min(index, parent.Children.Count), child);
    }

    private static void SetFilter(ComboBox box, string all, IEnumerable<string> options, string? selected)
    {
        var values = options.ToArray();
        var desired = new[] { all }.Concat(values).ToArray();
        if (!box.Items.Cast<object>().Select(item => item.ToString()).SequenceEqual(desired))
        {
            box.Items.Clear();
            foreach (var value in desired) box.Items.Add(value);
        }
        box.SelectedIndex = selected is null ? 0 : Math.Max(0, Array.IndexOf(values, selected) + 1);
    }

    private static StackPanel Section(string title)
    {
        var stack = Vertical(0);
        stack.Children.Add(new TextBlock { Text = title, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, Margin = new Thickness(12, 10, 12, 7) });
        return stack;
    }

    private static Border Card(UIElement content) => new() { Child = content, Background = new SolidColorBrush(global::Windows.UI.Color.FromArgb(20, 128, 128, 128)), BorderBrush = new SolidColorBrush(global::Windows.UI.Color.FromArgb(35, 128, 128, 128)), BorderThickness = new Thickness(1), CornerRadius = new CornerRadius(6), Padding = new Thickness(12) };
    private static Border Divider() => new() { Height = 1, Margin = new Thickness(0, 4, 0, 4), Background = new SolidColorBrush(global::Windows.UI.Color.FromArgb(32, 128, 128, 128)) };
    private static StackPanel Vertical(double spacing) => new() { Orientation = Orientation.Vertical, Spacing = spacing };
    private static StackPanel Horizontal(double spacing) => new() { Orientation = Orientation.Horizontal, Spacing = spacing, VerticalAlignment = VerticalAlignment.Center };
    private static ScrollViewer Scroll(UIElement content) => new() { Content = content, Padding = new Thickness(24), HorizontalScrollMode = ScrollMode.Disabled };
    private static TextBlock Empty(string text) => new() { Text = text, HorizontalAlignment = HorizontalAlignment.Center, Opacity = 0.6, Margin = new Thickness(18, 28, 18, 28) };
    private static StackPanel Labeled(string title, UIElement value)
    {
        var stack = Vertical(3);
        stack.Children.Add(new TextBlock { Text = title, FontSize = 12, Opacity = 0.6 });
        stack.Children.Add(value);
        return stack;
    }
    private static Image AssetImage(string source, double size) => new() { Source = new SvgImageSource(new Uri($"ms-appx:///{source}")), Width = size, Height = size };
    private static readonly SolidColorBrush Transparent = new(Microsoft.UI.Colors.Transparent);
    private static global::Windows.ApplicationModel.DataTransfer.DataPackage ClipboardContent(string value)
    {
        var package = new global::Windows.ApplicationModel.DataTransfer.DataPackage();
        package.SetText(value);
        return package;
    }

    private static async Task ExportAsync(RuntimeStore store, MainWindow window)
    {
        var picker = new FileSavePicker { SuggestedFileName = $"private-ai-gateway-usage-{DateTime.Now:yyyy-MM-dd}" };
        picker.FileTypeChoices.Add("CSV", new List<string> { ".csv" });
        InitializeWithWindow.Initialize(picker, WindowNative.GetWindowHandle(window));
        var file = await picker.PickSaveFileAsync();
        if (file is not null) await store.ExportUsageAsync(file.Path);
    }
}
