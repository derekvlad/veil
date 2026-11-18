/*
    SPDX-License-Identifier: MIT
*/

import QtQuick
import QtMultimedia
import org.kde.plasma.plasmoid

WallpaperItem {
    id: root

    // 0 = fill (crop), 1 = fit, 2 = stretch, 3 = centre
    readonly property int fillMode: root.configuration.FillMode

    Rectangle {
        anchors.fill: parent
        color: root.configuration.BackgroundColor
    }

    MediaPlayer {
        id: player
        source: root.configuration.VideoFile.length > 0
                ? "file://" + root.configuration.VideoFile
                : ""
        loops: MediaPlayer.Infinite
        muted: root.configuration.Muted
        videoOutput: output
        onErrorOccurred: (error, message) => console.warn("veil:", message)
    }

    VideoOutput {
        id: output
        anchors.fill: parent
        fillMode: {
            switch (root.fillMode) {
            case 1:
                return VideoOutput.PreserveAspectFit;
            case 2:
                return VideoOutput.Stretch;
            case 3:
                return VideoOutput.PreserveAspectFit;
            default:
                return VideoOutput.PreserveAspectCrop;
            }
        }
    }

    // Plasma hides the wallpaper while a fullscreen window is up; there is no
    // point in decoding frames nobody can see.
    onVisibleChanged: {
        if (visible) {
            player.play();
        } else {
            player.pause();
        }
    }

    Component.onCompleted: {
        if (root.configuration.VideoFile.length > 0) {
            player.play();
        }
    }

    Component.onDestruction: player.stop()
}
