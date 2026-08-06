# Plan d'implémentation: Favoris de modèles et Configuration Effort

## Partie 1: Favoris de modèles (priorité)

### 1.1 Stockage des favoris
- **Fichier**: `app/src/settings/mod.rs` ou nouveau fichier `app/src/ai/favorite_models.rs`
- **Structure**: Ajouter `favorite_models: Vec<LLMId>` dans les préférences utilisateur
- **API**: 
  - `add_favorite_model(llm_id: LLMId)`
  - `remove_favorite_model(llm_id: LLMId)`
  - `is_favorite_model(llm_id: LLMId) -> bool`
  - `get_favorite_models() -> Vec<LLMId>`

### 1.2 Modification du tri des modèles
- **Fichier**: `app/src/ai/execution_profiles/model_menu_items.rs`
- **Fonction**: `available_model_menu_items`
- **Changement**: Trier les choix pour mettre les favoris en premier
- **Logique**: 
  ```rust
  let favorite_ids = get_favorite_models();
  let mut choices: Vec<&LLMInfo> = choices.into_iter().collect();
  choices.sort_by(|a, b| {
      let a_is_fav = favorite_ids.contains(&a.id);
      let b_is_fav = favorite_ids.contains(&b.id);
      b_is_fav.cmp(&a_is_fav) // favoris en premier
  });
  ```

### 1.3 Ctrl+F pour favoriser
- **Fichier**: `app/src/settings_view/ai_page.rs`
- **Fonction**: Modifier `make_item_fields` pour ajouter hover state
- **Action**: Ajouter `on_hover` callback qui détecte Ctrl+F
- **Binding**: Ajouter keystate pour Ctrl+F dans le menu dropdown

### 1.4 F2 pour cycler les favoris
- **Fichier**: `app/src/settings_view/ai_page.rs`
- **Fonction**: Ajouter handler global pour F2
- **Logique**: 
  - Récupérer la liste des favoris
  - Trouver le modèle actuel
  - Sélectionner le favori suivant (cyclique)
  - Mettre à jour le dropdown

## Partie 2: Configuration Effort

### 2.1 Recherche de la configuration effort actuelle
- **Fichiers à examiner**:
  - `app/src/ai/llms.rs` - configuration des modèles
  - `app/src/ai/execution_profiles/` - profils d'exécution
  - `app/src/ai/blocklist/` - orchestration des agents

### 2.2 Ajout de l'UI de configuration
- **Fichier**: `app/src/settings_view/ai_page.rs`
- **Widget**: Nouveau widget pour la configuration effort
- **Options**: low, medium, high, xhigh, max
- **Placement**: Dans la section des modèles AI

### 2.3 Stockage de la configuration effort
- **Fichier**: `app/src/settings/mod.rs`
- **Structure**: Ajouter `model_effort: HashMap<LLMId, String>` ou valeur globale
- **API**: Getter/setter pour la configuration effort

### 2.4 Intégration avec les appels API
- **Fichier**: `app/src/ai/agent_sdk/` ou `app/src/ai/blocklist/`
- **Changement**: Passer la configuration effort dans les requêtes API
- **Format**: `output_config: {effort: "low"|"medium"|"high"|"xhigh"|"max"}`

## Ordre d'implémentation

1. Stockage des favoris (préférences utilisateur)
2. Tri des favoris dans les menus
3. Ctrl+F pour favoriser
4. F2 pour cycler les favoris
5. Recherche configuration effort existante
6. UI configuration effort
7. Intégration API effort

## Tests

- Test favoris: Ajouter/supprimer des favoris, vérifier le tri
- Test Ctrl+F: Favoriser un modèle en hover
- Test F2: Cycler entre les favoris
- Test effort: Configurer différents niveaux, vérifier les appels API
