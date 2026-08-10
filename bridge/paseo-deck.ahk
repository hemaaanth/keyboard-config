#Requires AutoHotkey v2.0
#SingleInstance Force
A_IconTip := "Paseo Deck"

; --- Config ---------------------------------------------------------------
; Adjust HelperPath if this repo lives somewhere else inside WSL.
PaseoExe := EnvGet("LOCALAPPDATA") "\Programs\Paseo\Paseo.exe"
HelperPath := "/home/system/Documents/Development/keyboard-config/bridge/paseo-deck.sh"
LogFile := A_ScriptDir "\paseo-deck.log"

activeSlot := 0
busy := false

; --- Helpers ----------------------------------------------------------------

FocusPaseo() {
    global PaseoExe
    if WinExist("ahk_exe Paseo.exe") {
        WinActivate("ahk_exe Paseo.exe")
    } else {
        Run(PaseoExe)
        WinWait("ahk_exe Paseo.exe", , 5)
        if WinExist("ahk_exe Paseo.exe")
            WinActivate("ahk_exe Paseo.exe")
    }
}

LogLine(text) {
    global LogFile
    try FileAppend(FormatTime(, "yyyy-MM-dd HH:mm:ss") " " text "`n", LogFile, "UTF-8")
}

; Runs the WSL helper hidden, shows the first output line as a tray tip,
; and logs it. Single-flight: drops the call if one is already in flight.
RunHelper(args) {
    global HelperPath, busy
    if busy {
        TrayTip("busy", "Paseo Deck")
        return
    }
    busy := true
    ; WScript.Shell.Exec flashes a console window for console apps, so run via
    ; cmd /c with hidden window and capture output through a temp file instead.
    outFile := A_Temp "\paseo-deck-out.txt"
    cmd := A_ComSpec ' /c wsl.exe -- "' HelperPath '" ' args ' > "' outFile '" 2>&1'
    try {
        Run(cmd, , "Hide", &pid)
        if ProcessWaitClose(pid, 15) {  ; nonzero = still running after timeout
            ProcessClose(pid)
            TrayTip("timeout", "Paseo Deck")
            LogLine(args " -> timeout")
            busy := false
            return
        }
        output := Trim(FileRead(outFile, "UTF-8"))
        firstLine := output != "" ? StrSplit(output, "`n", "`r")[1] : "(no output)"
        TrayTip(firstLine, "Paseo Deck")
        LogLine(args " -> " firstLine)
    } catch as e {
        TrayTip("error: " e.Message, "Paseo Deck")
        LogLine(args " -> error: " e.Message)
    }
    busy := false
}

JumpToSlot(slot) {
    global activeSlot
    activeSlot := slot
    FocusPaseo()
    ; Don't race the focus change: Ctrl+digit in the wrong window is a stray zoom
    ; or tab switch. Skip the jump if Paseo doesn't take focus in time.
    if WinWaitActive("ahk_exe Paseo.exe", , 2)
        Send("^" slot)
}

; --- Slot keys: F13..F21 -> workspace slot 1..9 -----------------------------
F13:: JumpToSlot(1)
F14:: JumpToSlot(2)
F15:: JumpToSlot(3)
F16:: JumpToSlot(4)
F17:: JumpToSlot(5)
F18:: JumpToSlot(6)
F19:: JumpToSlot(7)
F20:: JumpToSlot(8)
F21:: JumpToSlot(9)

; --- Slot 10: focus only, never send Ctrl+0 (that's zoom reset) ------------
F22:: {
    global activeSlot
    activeSlot := 10
    FocusPaseo()
}

; --- Approve / deny pending permission for the active slot ------------------
F23:: RunHelper("approve " (activeSlot > 0 ? activeSlot : ""))
F24:: RunHelper("deny " (activeSlot > 0 ? activeSlot : ""))

; --- Shift+F13..F16: commit / push / pr / merge on the active slot ---------
+F13:: SendAction("commit")
+F14:: SendAction("push")
+F15:: SendAction("pr")
+F16:: SendAction("merge")

SendAction(verb) {
    global activeSlot
    if activeSlot = 0 {
        TrayTip("press an agent key first", "Paseo Deck")
        return
    }
    RunHelper("action " activeSlot " " verb)
}

; --- Shift+F18/F19: thinking effort up/down on the active slot ---------------
+F18:: SendLevel("effort", "up")
+F19:: SendLevel("effort", "down")

; --- Shift+F20/F21: mode up (toward bypass) / down (toward plan) -------------
+F20:: SendLevel("mode", "up")
+F21:: SendLevel("mode", "down")

SendLevel(kind, dir) {
    global activeSlot
    if activeSlot = 0 {
        TrayTip("press an agent key first", "Paseo Deck")
        return
    }
    RunHelper(kind " " activeSlot " " dir)
}

; --- Shift+F24: focus Paseo only --------------------------------------------
+F24:: FocusPaseo()
