using Microsoft.Win32;
using System.ComponentModel;
using System.Runtime.InteropServices;

namespace PrivateAIGateway.Windows;

internal sealed class NativeTray : IDisposable
{
    private const uint CallbackMessage = 0x8001;
    private const uint Id = 1;
    private const uint NimAdd = 0;
    private const uint NimModify = 1;
    private const uint NimDelete = 2;
    private const uint NifMessage = 1;
    private const uint NifIcon = 2;
    private const uint NifTip = 4;
    private const uint WmLButtonUp = 0x0202;
    private const uint WmRButtonUp = 0x0205;
    private const uint MfString = 0;
    private const uint MfSeparator = 0x800;
    private const uint MfChecked = 8;
    private const uint MfGray = 1;
    private const uint TpmReturnCmd = 0x100;
    private const uint TpmRightButton = 2;
    private const uint ImageIcon = 1;
    private const uint LrLoadFromFile = 0x10;
    private static readonly nint HwndMessage = new(-3);

    private readonly nint ownerWindow;
    private readonly nint messageWindow;
    private readonly nint module;
    private readonly string windowClass;
    private readonly Action open;
    private readonly Action settings;
    private readonly Action toggle;
    private readonly Action quit;
    private readonly WindowProc callback;
    private readonly nint normalIcon;
    private readonly nint protectedIcon;
    private bool running;
    private string status = "Not protected";

    internal NativeTray(nint window, Action open, Action settings, Action toggle, Action quit)
    {
        ownerWindow = window;
        this.open = open;
        this.settings = settings;
        this.toggle = toggle;
        this.quit = quit;
        normalIcon = LoadIcon("tray.ico");
        protectedIcon = LoadIcon("tray-protected.ico");
        callback = WndProc;
        module = GetModuleHandle(null);
        windowClass = $"PrivateAIGateway.Tray.{Guid.NewGuid():N}";
        var definition = new WindowClass
        {
            WindowProcedure = Marshal.GetFunctionPointerForDelegate(callback),
            Instance = module,
            ClassName = windowClass,
        };
        if (RegisterClass(ref definition) == 0) ThrowLastWin32Error("register the tray window class");
        messageWindow = CreateWindowEx(0, windowClass, "Private AI Gateway Tray", 0, 0, 0, 0, 0, HwndMessage, 0, module, 0);
        if (messageWindow == 0)
        {
            var error = Marshal.GetLastWin32Error();
            UnregisterClass(windowClass, module);
            throw new Win32Exception(error, "Could not create the tray message window.");
        }
        Notify(NimAdd, normalIcon, "Private AI Gateway - Not protected");
    }

    internal void Update(bool running, bool protectedState, string status)
    {
        this.running = running;
        this.status = status;
        Notify(NimModify, protectedState ? protectedIcon : normalIcon, $"Private AI Gateway - {status}");
    }

    private nint WndProc(nint hwnd, uint message, nuint wParam, nint lParam)
    {
        if (message == CallbackMessage)
        {
            var action = (uint)(lParam.ToInt64() & 0xffff);
            if (action is WmLButtonUp or WmRButtonUp) ShowMenu();
            return 0;
        }
        return DefWindowProc(hwnd, message, wParam, lParam);
    }

    private void ShowMenu()
    {
        var menu = CreatePopupMenu();
        AppendMenu(menu, MfString | (running ? MfChecked : 0), 1, "Protected");
        AppendMenu(menu, MfString | MfGray, 2, status);
        AppendMenu(menu, MfSeparator, 0, null);
        AppendMenu(menu, MfString, 3, "Open Private AI Gateway");
        AppendMenu(menu, MfString, 4, "Settings…");
        AppendMenu(menu, MfSeparator, 0, null);
        AppendMenu(menu, MfString | (StartupManager.IsEnabled ? MfChecked : 0), 5, "Open at Login");
        AppendMenu(menu, MfSeparator, 0, null);
        AppendMenu(menu, MfString, 6, "Quit Private AI Gateway");
        GetCursorPos(out var point);
        SetForegroundWindow(ownerWindow);
        var command = TrackPopupMenu(menu, TpmReturnCmd | TpmRightButton, point.X, point.Y, 0, ownerWindow, 0);
        DestroyMenu(menu);
        switch (command)
        {
            case 1: toggle(); break;
            case 3: open(); break;
            case 4: settings(); break;
            case 5: StartupManager.SetEnabled(!StartupManager.IsEnabled); break;
            case 6: quit(); break;
        }
    }

    private void Notify(uint operation, nint icon, string tip)
    {
        var data = new NotifyIconData
        {
            Size = (uint)Marshal.SizeOf<NotifyIconData>(),
            Window = messageWindow,
            Id = Id,
            Flags = NifMessage | NifIcon | NifTip,
            CallbackMessage = CallbackMessage,
            Icon = icon,
            Tip = tip.Length > 127 ? tip[..127] : tip,
        };
        ShellNotifyIcon(operation, ref data);
    }

    private static nint LoadIcon(string name)
    {
        var path = Path.Combine(AppContext.BaseDirectory, "Assets", "brand", name);
        var icon = LoadImage(0, path, ImageIcon, 0, 0, LrLoadFromFile);
        if (icon == 0) throw new InvalidOperationException($"Cannot load tray icon {name}.");
        return icon;
    }

    public void Dispose()
    {
        Notify(NimDelete, normalIcon, "");
        DestroyWindow(messageWindow);
        UnregisterClass(windowClass, module);
        DestroyIcon(normalIcon);
        DestroyIcon(protectedIcon);
    }

    private static void ThrowLastWin32Error(string operation) =>
        throw new Win32Exception(Marshal.GetLastWin32Error(), $"Could not {operation}.");

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct NotifyIconData
    {
        public uint Size;
        public nint Window;
        public uint Id;
        public uint Flags;
        public uint CallbackMessage;
        public nint Icon;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string Tip;
        public uint State;
        public uint StateMask;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string Info;
        public uint TimeoutOrVersion;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 64)] public string InfoTitle;
        public uint InfoFlags;
        public Guid GuidItem;
        public nint BalloonIcon;
    }

    [StructLayout(LayoutKind.Sequential)] private struct Point { public int X; public int Y; }
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct WindowClass
    {
        public uint Style;
        public nint WindowProcedure;
        public int ClassExtra;
        public int WindowExtra;
        public nint Instance;
        public nint Icon;
        public nint Cursor;
        public nint Background;
        public string? MenuName;
        public string ClassName;
    }
    private delegate nint WindowProc(nint hwnd, uint message, nuint wParam, nint lParam);

    [DllImport("shell32.dll", EntryPoint = "Shell_NotifyIconW", CharSet = CharSet.Unicode)] private static extern bool ShellNotifyIcon(uint message, ref NotifyIconData data);
    [DllImport("user32.dll", EntryPoint = "LoadImageW", CharSet = CharSet.Unicode)] private static extern nint LoadImage(nint instance, string name, uint type, int cx, int cy, uint flags);
    [DllImport("user32.dll")] private static extern bool DestroyIcon(nint icon);
    [DllImport("kernel32.dll", EntryPoint = "GetModuleHandleW", CharSet = CharSet.Unicode)] private static extern nint GetModuleHandle(string? moduleName);
    [DllImport("user32.dll", EntryPoint = "RegisterClassW", CharSet = CharSet.Unicode, SetLastError = true)] private static extern ushort RegisterClass(ref WindowClass windowClass);
    [DllImport("user32.dll", EntryPoint = "UnregisterClassW", CharSet = CharSet.Unicode)] private static extern bool UnregisterClass(string className, nint instance);
    [DllImport("user32.dll", EntryPoint = "CreateWindowExW", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern nint CreateWindowEx(uint exStyle, string className, string windowName, uint style, int x, int y, int width, int height, nint parent, nint menu, nint instance, nint parameter);
    [DllImport("user32.dll", EntryPoint = "DefWindowProcW")] private static extern nint DefWindowProc(nint window, uint message, nuint wParam, nint lParam);
    [DllImport("user32.dll")] private static extern bool DestroyWindow(nint window);
    [DllImport("user32.dll")] private static extern nint CreatePopupMenu();
    [DllImport("user32.dll", EntryPoint = "AppendMenuW", CharSet = CharSet.Unicode)] private static extern bool AppendMenu(nint menu, uint flags, uint id, string? text);
    [DllImport("user32.dll")] private static extern bool DestroyMenu(nint menu);
    [DllImport("user32.dll")] private static extern uint TrackPopupMenu(nint menu, uint flags, int x, int y, int reserved, nint window, nint rect);
    [DllImport("user32.dll")] private static extern bool GetCursorPos(out Point point);
    [DllImport("user32.dll")] private static extern bool SetForegroundWindow(nint window);
}

internal static class StartupManager
{
    private const string Path = @"Software\Microsoft\Windows\CurrentVersion\Run";
    private const string Name = "Private AI Gateway";
    internal static bool IsEnabled => Registry.CurrentUser.OpenSubKey(Path)?.GetValue(Name) is string;
    internal static void SetEnabled(bool enabled)
    {
        using var key = Registry.CurrentUser.OpenSubKey(Path, true) ?? Registry.CurrentUser.CreateSubKey(Path);
        if (enabled) key.SetValue(Name, $"\"{Environment.ProcessPath}\" --autostart");
        else key.DeleteValue(Name, false);
    }
}
