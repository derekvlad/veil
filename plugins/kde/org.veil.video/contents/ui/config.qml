/*
    SPDX-License-Identifier: MIT
*/

import QtQuick
import QtQuick.Controls as QQC2
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

ColumnLayout {
    id: root

    property string cfg_VideoFile
    property int cfg_FillMode
    property bool cfg_Muted
    property color cfg_BackgroundColor

    Kirigami.FormLayout {
        Layout.fillWidth: true

        RowLayout {
            Kirigami.FormData.label: i18n("Video file:")

            QQC2.TextField {
                id: pathField
                Layout.fillWidth: true
                Layout.minimumWidth: Kirigami.Units.gridUnit * 18
                text: root.cfg_VideoFile
                placeholderText: "/home/user/Videos/wallpaper.mp4"
                onTextEdited: root.cfg_VideoFile = text
            }

            QQC2.Button {
                text: i18n("Browse…")
                onClicked: fileDialog.open()
            }
        }

        QQC2.ComboBox {
            id: modeBox
            Kirigami.FormData.label: i18n("Fill mode:")
            model: [i18n("Fill (crop)"), i18n("Fit"), i18n("Stretch"), i18n("Centre")]
            currentIndex: root.cfg_FillMode
            onActivated: root.cfg_FillMode = currentIndex
        }

        QQC2.CheckBox {
            text: i18n("Mute")
            checked: root.cfg_Muted
            onToggled: root.cfg_Muted = checked
        }

        QQC2.TextField {
            Kirigami.FormData.label: i18n("Background:")
            text: root.cfg_BackgroundColor
            onTextEdited: root.cfg_BackgroundColor = text
        }
    }

    QQC2.Dialog {
        id: fileDialog
        title: i18n("Choose a video")
        modal: true
        standardButtons: QQC2.Dialog.Ok | QQC2.Dialog.Cancel
        width: Kirigami.Units.gridUnit * 30

        contentItem: ColumnLayout {
            QQC2.TextField {
                id: dialogPath
                Layout.fillWidth: true
                text: root.cfg_VideoFile
                placeholderText: "/home/user/Videos/wallpaper.mp4"
            }
        }

        onAccepted: root.cfg_VideoFile = dialogPath.text
    }
}
