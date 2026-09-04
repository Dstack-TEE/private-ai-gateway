using Microsoft.UI.Xaml;
using System.Runtime.InteropServices;

namespace PrivateAIGateway.Windows;

public partial class App : Application
{
    public static MainWindow MainWindow { get; private set; } = null!;
    private static readonly Mutex Instance;
    private static readonly bool IsFirstInstance;

    static App()
    {
        Instance = new Mutex(true, "Local\\Dstack.PrivateAIGateway", out var firstInstance);
        IsFirstInstance = firstInstance;
    }

    public App()
    {
        UnhandledException += (_, args) => WriteCrashLog(args.Exception);
        InitializeComponent();
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        if (!IsFirstInstance)
        {
            NativeMethods.ActivateExistingWindow();
            Exit();
            return;
        }
        MainWindow = new MainWindow();
        Trace("app:activating");
        MainWindow.Activate();
        Trace("app:activated");
        if (Environment.GetCommandLineArgs().Contains("--autostart")) MainWindow.HideAfterLaunch();
    }

    private static void WriteCrashLog(Exception error)
    {
        try
        {
            var directory = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Private AI Gateway");
            Directory.CreateDirectory(directory);
            File.AppendAllText(Path.Combine(directory, "crash.log"), $"{DateTimeOffset.UtcNow:O}{Environment.NewLine}{error}{Environment.NewLine}{Environment.NewLine}");
        }
        catch { }
    }

    internal static void Trace(string stage)
    {
        var path = Environment.GetEnvironmentVariable("PRIVATE_AI_GATEWAY_LAUNCH_TRACE");
        if (string.IsNullOrWhiteSpace(path)) return;
        try { File.AppendAllText(path, $"{DateTimeOffset.UtcNow:O} {stage}{Environment.NewLine}"); }
        catch { }
    }

    private static class NativeMethods
    {
        [DllImport("user32.dll", EntryPoint = "FindWindowW", CharSet = CharSet.Unicode)]
        private static extern nint FindWindow(string? className, string windowName);

        [DllImport("user32.dll")]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool ShowWindow(nint window, int command);

        [DllImport("user32.dll")]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool SetForegroundWindow(nint window);

        internal static void ActivateExistingWindow()
        {
            var window = FindWindow(null, "Private AI Gateway");
            if (window == 0) return;
            ShowWindow(window, 9);
            SetForegroundWindow(window);
        }
    }
}
