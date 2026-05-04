# frozen_string_literal: true

class Api::V1::AccountsController < Api::V1::BaseController
  include Pagy::Backend

  before_action :ensure_read_scope, only: [ :index, :show ]
  before_action :ensure_write_scope, only: [ :create, :update, :destroy ]
  before_action :set_account, only: [ :show, :update, :destroy ]

  def index
    @per_page = safe_per_page_param

    @pagy, @accounts = pagy(
      accounts_scope.alphabetically,
      page: safe_page_param,
      limit: @per_page
    )

    render :index
  rescue => e
    Rails.logger.error "AccountsController#index error: #{e.message}"
    Rails.logger.error e.backtrace.join("\n")

    render json: {
      error: "internal_server_error",
      message: "An unexpected error occurred"
    }, status: :internal_server_error
  end

  def show
    render :show
  rescue => e
    Rails.logger.error "AccountsController#show error: #{e.message}"
    Rails.logger.error e.backtrace.join("\n")

    render json: {
      error: "internal_server_error",
      message: "An unexpected error occurred"
    }, status: :internal_server_error
  end

  def create
    family = current_resource_owner.family

    # Default currency to family currency if not provided
    params_with_defaults = account_params
    params_with_defaults[:currency] ||= family.currency

    @account = family.accounts.create_and_sync(params_with_defaults)

    if @account.persisted?
      @account.lock_saved_attributes!
      render :show, status: :created
    else
      render json: {
        error: "validation_failed",
        message: "Account could not be created",
        errors: @account.errors.full_messages
      }, status: :unprocessable_entity
    end
  rescue => e
    Rails.logger.error "AccountsController#create error: #{e.message}"
    Rails.logger.error e.backtrace.join("\n")

    render json: {
      error: "internal_server_error",
      message: "An unexpected error occurred"
    }, status: :internal_server_error
  end

  def update
    # Handle balance update if provided
    if account_params[:balance].present?
      result = @account.set_current_balance(account_params[:balance].to_d)
      unless result.success?
        render json: {
          error: "validation_failed",
          message: "Balance could not be updated",
          errors: [ result.error_message ]
        }, status: :unprocessable_entity
        return
      end
      @account.sync_later
    end

    # Update remaining account attributes
    update_params = account_params.except(:balance, :currency)
    if @account.update(update_params)
      @account.lock_saved_attributes!
      render :show
    else
      render json: {
        error: "validation_failed",
        message: "Account could not be updated",
        errors: @account.errors.full_messages
      }, status: :unprocessable_entity
    end
  rescue => e
    Rails.logger.error "AccountsController#update error: #{e.message}"
    Rails.logger.error e.backtrace.join("\n")

    render json: {
      error: "internal_server_error",
      message: "An unexpected error occurred"
    }, status: :internal_server_error
  end

  def destroy
    if @account.linked?
      render json: {
        error: "validation_failed",
        message: "Cannot delete linked account. Unlink the account first."
      }, status: :unprocessable_entity
      return
    end

    @account.destroy_later

    render json: {
      message: "Account deleted successfully"
    }, status: :ok
  rescue => e
    Rails.logger.error "AccountsController#destroy error: #{e.message}"
    Rails.logger.error e.backtrace.join("\n")

    render json: {
      error: "internal_server_error",
      message: "An unexpected error occurred"
    }, status: :internal_server_error
  end

  private

    def set_account
      unless valid_uuid?(params[:id])
        render json: {
          error: "not_found",
          message: "Account not found"
        }, status: :not_found
        return
      end

      @account = accounts_scope.find(params[:id])
    rescue ActiveRecord::RecordNotFound
      render json: {
        error: "not_found",
        message: "Account not found"
      }, status: :not_found
    end

    def ensure_read_scope
      authorize_scope!(:read)
    end

    def ensure_write_scope
      authorize_scope!(:write)
    end

    def accounts_scope
      scope = current_resource_owner.family.accounts
                                    .accessible_by(current_resource_owner)
                                    .includes(:accountable, account_providers: :provider)
      include_disabled_accounts? ? scope : scope.visible
    end

    def include_disabled_accounts?
      ActiveModel::Type::Boolean.new.cast(params[:include_disabled])
    end

    def valid_uuid?(value)
      value.to_s.match?(/\A[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\z/i)
    end

    def account_params
      params.require(:account).permit(
        :name, :balance, :subtype, :currency, :accountable_type,
        :institution_name, :institution_domain, :notes,
        accountable_attributes: {}
      )
    end

    def safe_page_param
      page = params[:page].to_i
      page > 0 ? page : 1
    end

    def safe_per_page_param
      per_page = params[:per_page].to_i

      case per_page
      when 1..100
        per_page
      else
        25
      end
    end
end
