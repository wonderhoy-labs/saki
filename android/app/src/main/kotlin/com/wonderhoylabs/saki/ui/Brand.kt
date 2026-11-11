// Shared by every Wonderhoy Labs app. Change it in wonderhoy-labs/template first.
package com.wonderhoylabs.saki.ui

import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.painter.Painter
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp

/**
 * The header every Wonderhoy Labs app opens with: its logo, its name in the brand colour, and what it does. The logo
 * is the project's own art, from the maintainer, usually `painterResource(R.drawable.logo)`. Without one the
 * header shows the name alone.
 */
@Composable
fun BrandHeader(subtitle: String, modifier: Modifier = Modifier, logo: Painter? = null) {
	Row(
		modifier = modifier,
		verticalAlignment = Alignment.CenterVertically,
		horizontalArrangement = Arrangement.spacedBy(16.dp),
	) {
		if (logo != null) {
			Image(
				painter = logo,
				contentDescription = null,
				contentScale = ContentScale.Crop,
				modifier = Modifier.size(56.dp).clip(RoundedCornerShape(16.dp)),
			)
		}
		Column {
			Text(
				text = Brand.NAME,
				style = MaterialTheme.typography.headlineMedium,
				fontWeight = FontWeight.SemiBold,
				color = MaterialTheme.colorScheme.primary,
			)
			Text(
				text = subtitle,
				style = MaterialTheme.typography.bodyMedium,
				color = MaterialTheme.colorScheme.onSurfaceVariant,
			)
		}
	}
}
