package com.wonderhoylabs.saki

import com.wonderhoylabs.saki.link.LinkStats
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

/** What the client service is up to. */
sealed interface ClientStatus {
    data object Idle : ClientStatus
    data class Connecting(val server: String) : ClientStatus
    data class Connected(val server: String, val stats: LinkStats) : ClientStatus
    data class Reconnecting(val server: String, val reason: String?) : ClientStatus
}

/** What the server service is up to. */
sealed interface ServerStatus {
    data object Stopped : ServerStatus
    data object Starting : ServerStatus
    data class Sharing(val name: String, val clients: List<ConnectedClient>) : ServerStatus
}

data class ConnectedClient(val id: Long, val name: String, val stats: LinkStats)

/** Where the services tell the screen what's happening. They all live in one process. */
object SakiState {
    private val clientFlow = MutableStateFlow<ClientStatus>(ClientStatus.Idle)
    private val serverFlow = MutableStateFlow<ServerStatus>(ServerStatus.Stopped)
    private val errorFlow = MutableStateFlow<String?>(null)

    val client: StateFlow<ClientStatus> = clientFlow
    val server: StateFlow<ServerStatus> = serverFlow
    val error: StateFlow<String?> = errorFlow

    fun setClient(status: ClientStatus) {
        clientFlow.value = status
    }

    fun setServer(status: ServerStatus) {
        serverFlow.value = status
    }

    fun setError(message: String?) {
        errorFlow.value = message
    }
}
