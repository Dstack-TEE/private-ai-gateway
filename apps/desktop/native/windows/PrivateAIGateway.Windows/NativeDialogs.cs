using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;

namespace PrivateAIGateway.Windows;

internal static class NativeDialogs
{
    internal static async Task ShowProfilesAsync(RuntimeStore store, XamlRoot root)
    {
        if (store.State.Profiles.Length == 0)
        {
            await ShowProfileEditorAsync(store, null, root);
            return;
        }
        while (true)
        {
            ConfidentialProfile? selected = store.ActiveProfile ?? store.State.Profiles.FirstOrDefault();
            string? nextAction = null;
            var list = new ListView { SelectionMode = ListViewSelectionMode.Single, MinHeight = 320, MaxHeight = 380 };
            foreach (var profile in store.State.Profiles)
            {
                var row = ProfileRow(profile, profile.Id == store.State.ActiveProfileId);
                row.Tag = profile;
                list.Items.Add(row);
                if (profile.Id == selected?.Id) list.SelectedItem = row;
            }
            list.SelectionChanged += (_, _) => selected = (list.SelectedItem as FrameworkElement)?.Tag as ConfidentialProfile;
            var actions = Horizontal(8);
            var add = new Button { Content = IconLabel("New", "\uE710") };
            var edit = new Button { Content = IconLabel("Edit", "\uE70F"), IsEnabled = selected is not null };
            var delete = new Button { Content = IconLabel("Delete", "\uE74D"), IsEnabled = selected is not null && store.State.Profiles.Length > 1 };
            var use = new Button { Content = "Use Profile", IsEnabled = selected?.VerifiedAt is not null && selected.Id != store.State.ActiveProfileId };
            add.Click += (_, _) => nextAction = "new";
            edit.Click += (_, _) => nextAction = "edit";
            delete.Click += (_, _) => nextAction = "delete";
            use.Click += (_, _) => nextAction = "use";
            actions.Children.Add(add); actions.Children.Add(edit); actions.Children.Add(delete); actions.Children.Add(new FrameworkElement { Width = 12 }); actions.Children.Add(use);
            var content = Vertical(12); content.Children.Add(list); content.Children.Add(actions);
            var dialog = Dialog("Profiles", content, root, "Done");
            foreach (var button in new[] { add, edit, delete, use }) button.Click += (_, _) => dialog.Hide();
            await dialog.ShowAsync();
            switch (nextAction)
            {
                case "new": await ShowProfileEditorAsync(store, null, root); break;
                case "edit" when selected is not null: await ShowProfileEditorAsync(store, selected, root); break;
                case "delete" when selected is not null:
                    if (await ConfirmAsync("Delete profile?", "The profile credential will be removed from Windows Credential Manager.", "Delete", root))
                        await store.DeleteProfileAsync(selected.Id);
                    break;
                case "use" when selected is not null: await store.ActivateProfileAsync(selected.Id); break;
                default: return;
            }
        }
    }

    private static async Task ShowProfileEditorAsync(RuntimeStore store, ConfidentialProfile? profile, XamlRoot root)
    {
        var name = new TextBox { Header = "Name", Text = profile?.Name ?? "RedPill" };
        var provider = new RadioButtons { Header = "Provider", MaxColumns = 3 };
        provider.Items.Add("Phala"); provider.Items.Add("RedPill"); provider.Items.Add("Custom");
        provider.SelectedIndex = profile?.Provider switch { "phala" => 0, "custom" => 2, _ => 1 };
        var endpoint = new TextBox { Header = "Endpoint", Text = profile?.RemoteUrl ?? "https://tee.redpill.ai", IsEnabled = provider.SelectedIndex == 2 };
        var key = new PasswordBox { Header = profile?.VerifiedAt is null ? "API key" : "API key (leave blank to keep)", PasswordRevealMode = PasswordRevealMode.Peek };
        var verify = new Button { Content = "Verify and Save", HorizontalAlignment = HorizontalAlignment.Right };
        var verified = new InfoBar { Severity = InfoBarSeverity.Success, Title = "Verified configuration", IsOpen = profile?.VerifiedAt is not null, IsClosable = false };
        var allowDev = new ToggleSwitch { Header = "Allow development OS", IsOn = !store.State.Config.RequireProductionOs };
        var explanation = new TextBlock { Text = "Development OS mode weakens the production attestation policy and is shown in yellow whenever protection is running.", TextWrapping = TextWrapping.Wrap, Opacity = 0.65 };
        var content = Vertical(14); content.Children.Add(name); content.Children.Add(provider); content.Children.Add(endpoint); content.Children.Add(key); content.Children.Add(verify); content.Children.Add(verified); content.Children.Add(allowDev); content.Children.Add(explanation);
        var dialog = Dialog(profile is null ? "New Profile" : "Edit Profile", content, root, "Cancel");
        provider.SelectionChanged += (_, _) =>
        {
            endpoint.IsEnabled = provider.SelectedIndex == 2;
            if (provider.SelectedIndex == 0) { endpoint.Text = "https://inference.phala.com"; if (profile is null) name.Text = "Phala"; }
            else if (provider.SelectedIndex == 1) { endpoint.Text = "https://tee.redpill.ai"; if (profile is null) name.Text = "RedPill"; }
            else if (profile is null) { endpoint.Text = ""; name.Text = "Custom"; }
        };
        verify.Click += async (_, _) =>
        {
            verify.IsEnabled = false;
            verify.Content = "Verifying…";
            var providerId = provider.SelectedIndex switch { 0 => "phala", 2 => "custom", _ => "redpill" };
            var input = new ConfidentialProfileInput(profile?.Id ?? $"profile-{Guid.NewGuid():N}", name.Text, providerId, endpoint.Text);
            if (await store.VerifyAndSaveAsync(input, allowDev.IsOn, key.Password)) dialog.Hide();
            verify.IsEnabled = true;
            verify.Content = "Verify and Save";
        };
        await dialog.ShowAsync();
    }

    internal static async Task ShowLocalApiAsync(RuntimeStore store, XamlRoot root)
    {
        var current = store.State.LocalApi;
        var address = new TextBox { Header = "Listen address", Text = current.ListenAddress };
        var network = new ToggleSwitch { Header = "Allow network access", IsOn = current.AllowNetworkAccess };
        var port = new NumberBox { Header = "Port", Value = current.Port, Minimum = 1024, Maximum = 65535, SpinButtonPlacementMode = NumberBoxSpinButtonPlacementMode.Compact };
        var host = new TextBox { Header = "Client host", Text = current.ClientHost ?? "" };
        var note = new InfoBar { Title = "Network access exposes the Local API beyond this PC.", Message = "Connected agents must be disconnected before changing the endpoint.", Severity = InfoBarSeverity.Warning, IsOpen = false, IsClosable = false };
        network.Toggled += (_, _) => note.IsOpen = network.IsOn;
        var content = Vertical(14); content.Children.Add(address); content.Children.Add(network); content.Children.Add(note); content.Children.Add(port); content.Children.Add(host);
        var dialog = new ContentDialog { Title = "Local API Settings", Content = content, PrimaryButtonText = "Save", CloseButtonText = "Cancel", DefaultButton = ContentDialogButton.Primary, XamlRoot = root, MinWidth = 540 };
        dialog.PrimaryButtonClick += async (_, args) =>
        {
            var deferral = args.GetDeferral();
            try
            {
                args.Cancel = true;
                var value = double.IsNaN(port.Value) ? 0 : port.Value;
                await store.SaveLocalApiAsync(new(address.Text, network.IsOn, (ushort)value, string.IsNullOrWhiteSpace(host.Text) ? null : host.Text));
                dialog.Hide();
            }
            catch { }
            finally { deferral.Complete(); }
        };
        await dialog.ShowAsync();
    }

    internal static async Task ShowProofAsync(GatewayState state, RequestActivity item, XamlRoot root)
    {
        var rows = Vertical(8);
        foreach (var row in new[]
        {
            ("Request", item.Id), ("Agent", item.Agent ?? "Unknown"), ("Model", item.Model ?? "Not reported"),
            ("Path", $"{item.Method} {item.Path}"), ("Status", item.Status.ToString()), ("Receipt", item.ReceiptId ?? "No receipt"),
            ("Policy", item.LocallyConstrained == true ? "Applied before forwarding" : "Not reported"),
            ("Rewrite", item.Rewritten == true ? "Service rewrote the request" : "No rewrite reported"),
            ("Delivery", item.LeftDevice ? "Request may have left this PC" : "Blocked locally before delivery"),
            ("Input tokens", item.InputTokens?.ToString("N0") ?? "Not reported"), ("Output tokens", item.OutputTokens?.ToString("N0") ?? "Not reported"),
            ("Cost", item.CostUsd?.ToString("C") ?? "Not reported"), ("Gateway keyset", state.Identity?.KeysetDigest ?? "Not available"),
            ("Detail", string.IsNullOrEmpty(item.Detail) ? "No additional detail" : item.Detail),
        }) rows.Children.Add(Labeled(row.Item1, row.Item2));
        var scroll = new ScrollViewer { Content = rows, MaxHeight = 530, HorizontalScrollMode = ScrollMode.Disabled };
        await Dialog(Verdict(item), scroll, root, "Done").ShowAsync();
    }

    internal static async Task ShowPrivacyAsync(GatewayState state, XamlRoot root)
    {
        var content = Vertical(18);
        content.Children.Add(new TextBlock { Text = "The gateway verifies the workload identity and model catalog before forwarding requests. Each response receipt binds the request, verified upstream session, and returned response.", TextWrapping = TextWrapping.Wrap });
        if (state.Identity is { } identity)
        {
            content.Children.Add(Heading("Workload identity"));
            foreach (var row in new[] { ("TEE", identity.TeeType), ("Trust level", identity.TrustLevel), ("Keyset digest", identity.KeysetDigest), ("Serving mode", identity.Serving), ("TLS SPKI", identity.TlsSpki ?? "Not published") }) content.Children.Add(Labeled(row.Item1, row.Item2));
            content.Children.Add(Heading("Source provenance"));
            foreach (var row in new[] { ("Repository", identity.Source.RepoUrl ?? "Not published"), ("Commit", identity.Source.RepoCommit ?? "Not published"), ("Image digest", identity.Source.ImageDigest ?? "Not published") }) content.Children.Add(Labeled(row.Item1, row.Item2));
        }
        content.Children.Add(Heading("Verification checks"));
        foreach (var check in state.Checks)
        {
            var row = Horizontal(10); row.Children.Add(new FontIcon { Glyph = check.Status == "pass" ? "\uE73E" : check.Status == "fail" ? "\uE711" : "\uE946" });
            var text = Vertical(2); text.Children.Add(new TextBlock { Text = check.Title }); text.Children.Add(new TextBlock { Text = check.Detail, FontSize = 12, Opacity = 0.65, TextWrapping = TextWrapping.Wrap }); row.Children.Add(text); content.Children.Add(row);
        }
        await Dialog("Privacy Verification", new ScrollViewer { Content = content, MaxHeight = 560, HorizontalScrollMode = ScrollMode.Disabled }, root, "Done").ShowAsync();
    }

    internal static async Task<bool> ConfirmAsync(string title, string message, string action, XamlRoot root)
    {
        var dialog = new ContentDialog { Title = title, Content = message, PrimaryButtonText = action, CloseButtonText = "Cancel", DefaultButton = ContentDialogButton.Close, XamlRoot = root };
        return await dialog.ShowAsync() == ContentDialogResult.Primary;
    }

    private static ContentDialog Dialog(string title, UIElement content, XamlRoot root, string close) => new() { Title = title, Content = content, CloseButtonText = close, XamlRoot = root, MinWidth = 560 };
    private static StackPanel Vertical(double spacing) => new() { Orientation = Orientation.Vertical, Spacing = spacing };
    private static StackPanel Horizontal(double spacing) => new() { Orientation = Orientation.Horizontal, Spacing = spacing, VerticalAlignment = VerticalAlignment.Center };
    private static StackPanel IconLabel(string text, string glyph) { var row = Horizontal(6); row.Children.Add(new FontIcon { Glyph = glyph, FontSize = 14 }); row.Children.Add(new TextBlock { Text = text }); return row; }
    private static TextBlock Heading(string text) => new() { Text = text, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, Margin = new Thickness(0, 8, 0, 0) };
    private static Grid Labeled(string label, string value) { var grid = new Grid { ColumnSpacing = 18 }; grid.ColumnDefinitions.Add(new() { Width = new GridLength(145) }); grid.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) }); grid.Children.Add(new TextBlock { Text = label, Opacity = 0.65, HorizontalAlignment = HorizontalAlignment.Right }); var text = new TextBlock { Text = value, TextWrapping = TextWrapping.Wrap, IsTextSelectionEnabled = true }; Grid.SetColumn(text, 1); grid.Children.Add(text); return grid; }

    private static StackPanel ProfileRow(ConfidentialProfile profile, bool active)
    {
        var row = Horizontal(12);
        row.Children.Add(ProviderImage(profile.Provider));
        var labels = Vertical(2); labels.Children.Add(new TextBlock { Text = profile.Name, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold }); labels.Children.Add(new TextBlock { Text = profile.RemoteUrl, FontSize = 12, Opacity = 0.65 }); row.Children.Add(labels);
        if (profile.VerifiedAt is not null) row.Children.Add(Badge("Verified"));
        if (active) row.Children.Add(new FontIcon { Glyph = "\uE73E" });
        return row;
    }

    private static Image ProviderImage(string provider)
    {
        var source = provider switch { "phala" => "providers/phala.svg", "redpill" => "providers/redpill.png", _ => null };
        if (source is null) return new Image { Width = 28, Height = 28 };
        ImageSource image = source.EndsWith(".svg") ? new SvgImageSource(new Uri($"ms-appx:///Assets/{source}")) : new BitmapImage(new Uri($"ms-appx:///Assets/{source}"));
        return new Image { Source = image, Width = 28, Height = 28 };
    }

    private static string Verdict(RequestActivity item) => item.Verified == true ? "Proof verified" : !item.LeftDevice ? "Blocked locally" : item.Verified == false ? "Proof failed" : "Proof unavailable";
    private static Border Badge(string text) => new() { Background = new SolidColorBrush(global::Windows.UI.Color.FromArgb(36, 44, 110, 73)), CornerRadius = new CornerRadius(4), Padding = new Thickness(7, 3, 7, 3), Child = new TextBlock { Text = text, Foreground = new SolidColorBrush(global::Windows.UI.Color.FromArgb(255, 44, 110, 73)), FontSize = 12 } };
}
